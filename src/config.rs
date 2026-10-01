//! `~/.config/tmdview/config.toml`: defaults for the command line and settings for
//! plugins. Every part is optional, and so is the file.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::{Direction, Theme};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: Option<Theme>,
    pub split: Option<Direction>,
    pub size: Option<f32>,
    pub watch: Option<bool>,
    pub poll: bool,
    /// Each plugin's table, which its script reads as `tmdview.config.<name>`.
    pub plugins: BTreeMap<String, toml::Table>,
}

impl Config {
    /// The config file, or the defaults when there is none.
    pub fn load() -> Result<Self> {
        let Some(path) = path() else { return Ok(Self::default()) };
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("reading {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// The plugins' tables as one JSON object.
    pub fn plugins_json(&self) -> String {
        serde_json::to_string(&self.plugins).expect("TOML tables are valid JSON")
    }
}

/// `$XDG_CONFIG_HOME/tmdview/config.toml`, or `~/.config/tmdview/config.toml`.
fn path() -> Option<PathBuf> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(dir.join("tmdview").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_part() {
        let config = Config::parse(
            r##"
theme = "terminal"
split = "right"
size = 0.4
watch = false

[plugins.mermaid]
theme = "forest"
"##,
        )
        .unwrap();
        assert!(matches!(config.theme, Some(Theme::Terminal)));
        assert!(matches!(config.split, Some(Direction::Right)));
        assert_eq!(config.watch, Some(false));
        assert_eq!(config.plugins_json(), r#"{"mermaid":{"theme":"forest"}}"#);
    }

    #[test]
    fn an_empty_file_changes_nothing() {
        let config = Config::parse("").unwrap();
        assert!(config.theme.is_none());
        assert_eq!(config.plugins_json(), "{}");
    }

    #[test]
    fn rejects_typos() {
        assert!(Config::parse("thme = \"dark\"").is_err());
        assert!(Config::parse("theme = \"blue\"").is_err());
    }
}
