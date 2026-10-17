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
