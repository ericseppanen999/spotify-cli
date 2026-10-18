//! A local, loopback-only web player: a tiny HTTP server hands the Spotify Web
//! Playback SDK a fresh access token, so it can register itself as a Connect
//! device that the rest of this app already knows how to control.
use crate::auth::{http_client, Auth};
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const PLAYER_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Spot Web Player</title></head>
<body style="background:#101419;color:#89919b;font-family:monospace;padding:2rem">
<h1 style="color:#1DB954">Spot CLI</h1>
<p id="status">Connecting to Spotify...</p>
<script src="https://sdk.scdn.co/spotify-player.js"></script>
<script>
window.onSpotifyWebPlaybackSDKReady = () => {
  const status = m => { document.getElementById('status').textContent = m; };
  const player = new Spotify.Player({
    name: 'Spot CLI',
    getOAuthToken: cb => fetch('/token').then(r => r.json()).then(d => cb(d.token)),
    volume: 0.5,
  });
  player.addListener('ready', () => {
    status('Ready. Pick "Spot CLI" from the device list (d) in the terminal app, then control playback from there. Keep this window open.');
  });
  player.addListener('not_ready', () => status('Disconnected.'));
  player.addListener('initialization_error', ({ message }) => status('Initialization error: ' + message));
  player.addListener('authentication_error', ({ message }) => status('Authentication error: ' + message));
  player.addListener('account_error', ({ message }) => status('Account error (Spotify Premium is required): ' + message));
  player.connect();
};
</script>
</body>
</html>"#;

async fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
}

async fn handle(stream: &mut TcpStream, auth: &mut Auth, http: &reqwest::Client) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.windows(4).any(|w| w == b"\r\n\r\n") || buffer.len() > 8192 {
                    break;
                }
            }
        }
    }
    let request = String::from_utf8_lossy(&buffer);
    let target = request
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("GET "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or("");
    match target {
        "/" => respond(stream, "200 OK", "text/html; charset=utf-8", PLAYER_HTML).await,
        "/token" => {
            if auth.tokens.expires_at <= Utc::now().timestamp() + 30 {
                let _ = auth.refresh(http).await;
            }
            let body = json!({ "token": auth.tokens.access_token }).to_string();
            respond(stream, "200 OK", "application/json", &body).await;
        }
        _ => respond(stream, "404 Not Found", "text/plain", "not found").await,
    }
}

/// Starts the loopback token/player server in the background and returns its port.
pub async fn serve(client_id: String, dir: PathBuf) -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let http = http_client()?;
    let mut auth = Auth::load(&client_id, &dir)?;
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            handle(&mut stream, &mut auth, &http).await;
        }
    });
    Ok(port)
}

/// Opens the player page in a lightweight Edge app window (no tabs/toolbar).
/// Fire-and-forget: if Edge isn't on this machine, the caller's message
/// includes the URL so the user can open it in any browser instead.
pub fn open(port: u16) {
    let url = format!("http://127.0.0.1:{port}");
    let _ = Command::new("cmd")
        .args(["/C", "start", "", "msedge", &format!("--app={url}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn();
}
#[cfg(test)]
mod tests {
    use super::*;

    async fn get(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test]
    async fn serves_player_page_and_token() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tokens.json"),
            json!({
                "access_token": "abc123",
                "refresh_token": "refresh",
                "expires_at": Utc::now().timestamp() + 3600
            })
            .to_string(),
        )
        .unwrap();
        let port = serve("client".into(), dir.path().to_path_buf())
            .await
            .unwrap();

        let page = get(port, "/").await;
        assert!(page.contains("200 OK"));
        assert!(page.contains("spotify-player.js"));

        let token = get(port, "/token").await;
        assert!(token.contains("200 OK"));
        assert!(token.contains("abc123"));

        let missing = get(port, "/nope").await;
        assert!(missing.contains("404"));
    }
}
