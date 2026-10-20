//! The background process that owns Up Next, so the queue keeps advancing
//! (and survives) after the TUI exits. Started on demand as `spot daemon`,
//! it keeps the queue on disk, hands songs to Spotify as they come due, and
//! exits once the queue has been empty for a while. The TUI talks to it over
//! a loopback socket: one JSON request line and one JSON reply line per
//! connection.
use crate::{
    api::{Api, Track},
    queue::Queue,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

/// Next to the web player's port. Binding it doubles as the single-instance
/// lock: a second daemon finds it taken and exits.
pub const PORT: u16 = 47824;
const POLL: Duration = Duration::from_secs(5);
/// How long an empty queue keeps the daemon around: long enough for the
/// request that spawned it to arrive, and to absorb remove-then-re-add.
const IDLE_EXIT: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Get,
    Add { track: Track },
    Remove { index: usize },
    Shift { index: usize, delta: isize },
    PlayNow { index: usize },
    Next,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub queue: Queue,
    /// From this request, or else the last background hand-off failure.
    pub error: Option<String>,
}

fn queue_path(dir: &Path) -> PathBuf {
    dir.join("queue.json")
}
/// The queue as last saved; empty if there's no file or it's unreadable.
pub fn load(dir: &Path) -> Queue {
    std::fs::read(queue_path(dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
fn save(dir: &Path, queue: &Queue) -> Result<()> {
    let path = queue_path(dir);
    if queue.is_empty() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    } else {
        Ok(std::fs::write(path, serde_json::to_vec(queue)?)?)
    }
}

pub async fn run(mut api: Api, dir: PathBuf) -> Result<()> {
    let Ok(listener) = TcpListener::bind(("127.0.0.1", PORT)).await else {
        return Ok(());
    };
    let mut queue = load(&dir);
    let mut playing: Option<String> = None;
    let mut last_error: Option<String> = None;
    let mut last_edit = Instant::now();
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                if serve(stream, &mut api, &mut queue, &mut last_error).await {
                    last_edit = Instant::now();
                    let _ = save(&dir, &queue);
                }
            }
            _ = poll.tick(), if !queue.is_empty() => {
                let result = match api.playback().await {
                    Ok(playback) => {
                        let uri = playback.item.as_ref().map(|t| t.uri.clone());
                        let changed = uri != playing;
                        playing = uri;
                        queue.sync(&mut api, &playback, changed).await
                    }
                    Err(e) => Err(e),
                };
                match result {
                    Ok(true) => {
                        let _ = save(&dir, &queue);
                    }
                    Ok(false) => {}
                    Err(e) => last_error = Some(e.to_string()),
                }
            }
            _ = tokio::time::sleep(IDLE_EXIT), if queue.is_empty() => {}
        }
        if queue.is_empty() && last_edit.elapsed() >= IDLE_EXIT {
            let _ = save(&dir, &queue);
            return Ok(());
        }
    }
}

/// Handles one connection. Returns whether the queue was edited.
async fn serve(
    stream: TcpStream,
    api: &mut Api,
    queue: &mut Queue,
    last_error: &mut Option<String>,
) -> bool {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let mut reader = BufReader::new(read.take(1 << 16));
    let read = reader.read_line(&mut line);
    if !matches!(
        tokio::time::timeout(Duration::from_secs(2), read).await,
        Ok(Ok(_))
    ) {
        return false;
    }
    let (edited, result) = match serde_json::from_str::<Request>(&line) {
        Err(e) => (false, Err(e.into())),
        Ok(request) => {
            let edited = !matches!(request, Request::Get);
            let result = match request {
                Request::Get => Ok(()),
                Request::Add { track } => {
                    queue.add(track);
                    Ok(())
                }
                Request::Remove { index } => {
                    queue.remove(index);
                    Ok(())
                }
                Request::Shift { index, delta } => {
                    queue.shift(index, delta);
                    Ok(())
                }
                Request::PlayNow { index } => queue.play_now(api, index).await,
                Request::Next => queue.next(api).await,
            };
            (edited, result)
        }
    };
    let reply = Reply {
        queue: queue.clone(),
        error: match result {
            Err(e) => Some(e.to_string()),
            Ok(()) => last_error.take(),
        },
    };
    if let Ok(mut body) = serde_json::to_vec(&reply) {
        body.push(b'\n');
        let _ = write.write_all(&body).await;
    }
    edited
}

async fn call_at(port: u16, request: &Request) -> Result<Reply> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let mut body = serde_json::to_vec(request)?;
    body.push(b'\n');
    stream.write_all(&body).await?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await?;
    serde_json::from_str(&line).context("Unexpected reply from the queue daemon")
}
/// Sends `request` to a running daemon; fails if none is running.
pub async fn call(request: &Request) -> Result<Reply> {
    call_at(PORT, request).await
}
/// Like [`call`], but starts the daemon first if it isn't running.
pub async fn call_spawning(request: &Request) -> Result<Reply> {
    if let Ok(reply) = call(request).await {
        return Ok(reply);
    }
    spawn()?;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(reply) = call(request).await {
            return Ok(reply);
        }
    }
    anyhow::bail!("Couldn't start the queue daemon (spot daemon)")
}
/// Starts `spot daemon` detached from this terminal, so closing or Ctrl+C-ing
/// it doesn't take the daemon down too.
pub fn spawn() -> Result<()> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Auth;
    use serde_json::json;

    #[tokio::test]
    async fn serves_edits_over_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tokens.json"),
            json!({"access_token":"a","refresh_token":"r","expires_at":i64::MAX}).to_string(),
        )
        .unwrap();
        let mut api = Api::new(Auth::load("client", dir.path()).unwrap()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut queue = Queue::default();
            let mut last_error = None;
            let mut edits = 0;
            for _ in 0..4 {
                let (stream, _) = listener.accept().await.unwrap();
                edits += serve(stream, &mut api, &mut queue, &mut last_error).await as usize;
            }
            edits
        });
        for uri in ["a", "b"] {
            let track = Track {
                uri: uri.into(),
                ..Default::default()
            };
            call_at(port, &Request::Add { track }).await.unwrap();
        }
        call_at(port, &Request::Shift { index: 0, delta: 1 })
            .await
            .unwrap();
        let reply = call_at(port, &Request::Get).await.unwrap();
        let uris: Vec<_> = reply.queue.pending.iter().map(|t| t.uri.as_str()).collect();
        assert_eq!(uris, ["b", "a"]);
        assert!(reply.error.is_none());
        assert_eq!(server.await.unwrap(), 3);
    }

    #[test]
    fn saved_queue_round_trips_and_clears_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue = Queue::default();
        queue.add(Track {
            uri: "a".into(),
            ..Default::default()
        });
        save(dir.path(), &queue).unwrap();
        assert_eq!(load(dir.path()).pending.len(), 1);
        queue.remove(0);
        save(dir.path(), &queue).unwrap();
        assert!(!queue_path(dir.path()).exists());
        assert!(load(dir.path()).is_empty());
    }
}
