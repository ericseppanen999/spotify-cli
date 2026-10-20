use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use spotify_cli::{
    api::{self, Api},
    auth::{self, Auth},
    config::{self, Config},
    daemon, newmusic, tui,
};

#[derive(Parser)]
#[command(
    name = "spot",
    version,
    about = "Spotify remote and weekly music diary"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Sign in with Spotify (no client secret required)
    Login {
        #[arg(long)]
        client_id: Option<String>,
    },
    Play {
        /// Track, album, or playlist URI; omit to resume
        uri: Option<String>,
    },
    Pause,
    Next,
    Prev,
    Devices,
    Transfer {
        device: String,
    },
    Now,
    Search {
        query: String,
        #[arg(long, value_enum, default_value = "track")]
        kind: Kind,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Total results to fetch across Spotify's pages
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u8).range(1..=100))]
        limit: u8,
    },
    Recent,
    /// List playlists, or the tracks in a playlist
    Playlists {
        playlist: Option<String>,
    },
    Playlist {
        #[command(subcommand)]
        command: PlaylistCommand,
    },
    /// Add a track; omit playlist when new music mode is on
    Add {
        track: String,
        playlist: Option<String>,
    },
    Queue {
        track: String,
    },
    Like {
        track: Option<String>,
    },
    Volume {
        #[arg(value_parser=clap::value_parser!(u8).range(0..=100))]
        percent: u8,
    },
    Newmusic {
        #[arg(value_enum)]
        mode: Option<Mode>,
    },
    /// Print config directory (contains config.toml and tokens.json)
    Config,
    /// Open the keyboard-driven terminal interface
    Tui,
    /// Background process that keeps Up Next going after the TUI exits;
    /// started automatically when needed.
    #[command(hide = true)]
    Daemon,
}
#[derive(Subcommand)]
enum PlaylistCommand {
    New { name: String },
}
#[derive(Clone, ValueEnum)]
enum Kind {
    Track,
    Album,
    Artist,
}
#[derive(Clone, ValueEnum)]
enum Mode {
    On,
    Off,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let cli = Cli::parse();
    let dir = config::directory()?;
    let mut config = Config::load(&dir)?;
    let command = cli.command.unwrap_or(Command::Tui);
    match &command {
        Command::Config => {
            println!("{}", dir.display());
            return Ok(());
        }
        Command::Login { client_id } => {
            let id = client_id
                .clone()
                .or_else(|| std::env::var("SPOT_CLIENT_ID").ok())
                .unwrap_or(config.client_id.clone());
            auth::login(&id, &dir).await?;
            config.client_id = id;
            config.save(&dir)?;
            println!("Signed in. Run spot devices or spot tui.");
            return Ok(());
        }
        Command::Newmusic { mode: None } => {
            println!(
                "New music mode: {}",
                if config.newmusic { "on" } else { "off" }
            );
            return Ok(());
        }
        Command::Newmusic {
            mode: Some(Mode::Off),
        } => {
            config.newmusic = false;
            config.save(&dir)?;
            println!("New music mode: off");
            return Ok(());
        }
        _ => {}
    }
    let mut api = Api::new(Auth::load(&config.client_id, &dir)?)?;
    match command {
        Command::Play { uri } => {
            let uri = uri.map(|s| normalize_play_uri(&s)).transpose()?;
            api.play(uri.as_deref()).await?;
        }
        Command::Pause => api.pause().await?,
        Command::Next => api.next().await?,
        Command::Prev => api.previous().await?,
        Command::Devices => {
            for d in api.devices().await? {
                println!(
                    "{} {}  {}{}",
                    if d.is_active { "*" } else { " " },
                    d.id.as_deref().unwrap_or("unavailable"),
                    d.name,
                    if d.is_restricted { " (restricted)" } else { "" }
                );
            }
        }
        Command::Transfer { device } => {
            let devices = api.devices().await?;
            let matches: Vec<_> = devices
                .iter()
                .filter(|d| {
                    d.id.as_deref() == Some(&device) || d.name.eq_ignore_ascii_case(&device)
                })
                .collect();
            anyhow::ensure!(
                matches.len() == 1,
                "Device not found or ambiguous; use an ID from spot devices"
            );
            anyhow::ensure!(
                !matches[0].is_restricted,
                "This device does not allow remote control"
            );
            api.transfer(
                matches[0]
                    .id
                    .as_deref()
                    .context("Device has no transferable ID")?,
            )
            .await?;
        }
        Command::Now => {
            let p = api.playback().await?;
            if let Some(t) = p.item {
                println!(
                    "{}  {}  [{}]",
                    if p.is_playing { "Playing" } else { "Paused" },
                    t.label(),
                    p.device.map(|d| d.name).unwrap_or_default()
                );
            } else {
                println!("Nothing playing. Open Spotify on a device to start.");
            }
        }
        Command::Search {
            query,
            kind,
            offset,
            limit,
        } => {
            let kind = match kind {
                Kind::Track => "track",
                Kind::Album => "album",
                Kind::Artist => "artist",
            };
            let items = api
                .search_results(&query, kind, offset, usize::from(limit))
                .await?;
            for item in items {
                println!(
                    "{}  {}",
                    item["uri"].as_str().unwrap_or(""),
                    item["name"].as_str().unwrap_or("")
                );
            }
        }
        Command::Recent => print_tracks(api.recent().await?),
        Command::Playlists { playlist: Some(id) } => print_tracks(api.playlist_tracks(&id).await?),
        Command::Playlists { playlist: None } => {
            for p in api.playlists().await? {
                println!("{}  {}", p.id, p.name);
            }
        }
        Command::Playlist {
            command: PlaylistCommand::New { name },
        } => {
            let p = api.create_playlist(&name, "").await?;
            println!("{}  {}", p.id, p.name);
        }
        Command::Add { track, playlist } => {
            api::track_uri(&track)?;
            let playlist = match playlist {
                Some(p) => p,
                None if config.newmusic => newmusic::ensure(&mut api, &mut config, &dir).await?,
                None => anyhow::bail!("Specify a playlist or enable spot newmusic on"),
            };
            api.add(&track, &playlist).await?;
            println!("Added to playlist {playlist}");
        }
        Command::Queue { track } => api.queue(&track).await?,
        Command::Like { track } => {
            let track = match track {
                Some(t) => t,
                None => api.playback().await?.item.context("Nothing playing")?.uri,
            };
            api.like(&track).await?;
            println!("Saved to your library");
        }
        Command::Volume { percent } => api.volume(percent).await?,
        Command::Newmusic {
            mode: Some(Mode::On),
        } => {
            let id = newmusic::ensure(&mut api, &mut config, &dir).await?;
            config.newmusic = true;
            config.save(&dir)?;
            println!("New music mode: on ({id})");
        }
        Command::Tui => tui::run(api, config, dir).await?,
        Command::Daemon => daemon::run(api, dir).await?,
        _ => unreachable!(),
    }
    Ok(())
}
fn normalize_play_uri(input: &str) -> Result<String> {
    for kind in ["track", "album", "playlist"] {
        if let Ok(id) = api::spotify_id(input, kind) {
            return Ok(format!("spotify:{kind}:{id}"));
        }
    }
    anyhow::bail!("Expected a track, album, or playlist URI/share URL")
}
fn print_tracks(tracks: Vec<api::Track>) {
    for t in tracks {
        println!("{}  {}", t.uri, t.label());
    }
}
