use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub client_id: String,
    pub newmusic: bool,
    pub weeks: BTreeMap<String, String>,
    pub theme: Theme,
    pub keys: BTreeMap<String, String>,
    pub pins: Vec<Pin>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    Track,
    Playlist,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub kind: PinKind,
    pub uri: String,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub accent: String,
    pub dim: String,
    pub highlight: String,
    pub error: String,
}
impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: "#1DB954".into(),
            dim: "#89919b".into(),
            highlight: "#253d30".into(),
            error: "#ff6b6b".into(),
        }
    }
}
impl Theme {
    pub fn presets() -> Vec<(&'static str, Theme)> {
        vec![
            ("Spotify", Theme::default()),
            (
                "Dracula",
                Theme {
                    accent: "#bd93f9".into(),
                    dim: "#6272a4".into(),
                    highlight: "#44475a".into(),
                    error: "#ff5555".into(),
                },
            ),
            (
                "Nord",
                Theme {
                    accent: "#88c0d0".into(),
                    dim: "#4c566a".into(),
                    highlight: "#3b4252".into(),
                    error: "#bf616a".into(),
                },
            ),
            (
                "Gruvbox",
                Theme {
                    accent: "#fabd2f".into(),
                    dim: "#928374".into(),
                    highlight: "#3c3836".into(),
                    error: "#fb4934".into(),
                },
            ),
            (
                "Solarized",
                Theme {
                    accent: "#268bd2".into(),
                    dim: "#586e75".into(),
                    highlight: "#073642".into(),
                    error: "#dc322f".into(),
                },
            ),
        ]
    }
}
pub fn directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SPOT_CONFIG_DIR") {
        return Ok(path.into());
    }
    Ok(directories::ProjectDirs::from("", "", "spotify-cli")
        .context("Cannot locate config directory")?
        .config_dir()
        .into())
}
impl Config {
    pub fn toggle_pin(&mut self, pin: Pin) -> bool {
        if let Some(index) = self.pins.iter().position(|p| p.uri == pin.uri) {
            self.pins.remove(index);
            false
        } else {
            self.pins.push(pin);
            true
        }
    }
    pub fn load(dir: &Path) -> Result<Self> {
        match fs::read_to_string(dir.join("config.toml")) {
            Ok(s) => toml::from_str(&s).context("Invalid config.toml"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, dir: &Path) -> Result<()> {
        write_private(
            &dir.join("config.toml"),
            toml::to_string_pretty(self)?.as_bytes(),
        )
    }
}
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().context("Missing parent directory")?)?;
    let temp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, path)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pins_persist_and_toggle_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mut config: Config = toml::from_str("client_id = 'existing'").unwrap();
        assert!(config.pins.is_empty());
        let song = Pin {
            kind: PinKind::Track,
            uri: "spotify:track:test".into(),
            name: "Song — Artist".into(),
        };
        let playlist = Pin {
            kind: PinKind::Playlist,
            uri: "spotify:playlist:test".into(),
            name: "Favorites".into(),
        };
        assert!(config.toggle_pin(song.clone()));
        assert!(config.toggle_pin(playlist.clone()));
        config.save(dir.path()).unwrap();
        let mut loaded = Config::load(dir.path()).unwrap();
        assert_eq!(loaded.pins, vec![song.clone(), playlist.clone()]);
        assert!(!loaded.toggle_pin(Pin {
            name: "Renamed".into(),
            ..song
        }));
        assert_eq!(loaded.pins, vec![playlist]);
        assert_eq!(loaded.client_id, "existing");
        loaded.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()).unwrap().pins.len(), 1);
    }
    #[test]
    fn config_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config {
            client_id: "test".into(),
            newmusic: true,
            ..Default::default()
        };
        c.save(dir.path()).unwrap();
        c.save(dir.path()).unwrap();
        assert!(Config::load(dir.path()).unwrap().newmusic);
        assert!(toml::from_str::<Config>("newmusic = 'yes'").is_err());
    }
}
