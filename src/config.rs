//! `~/.config/tmdview/config.toml`: defaults for the command line, code colors, and
//! settings for plugins. Every part is optional, and so is the file.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
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
    pub colors: Colors,
    /// Each plugin's table, which its script reads as `tmdview.config.<name>`.
    pub plugins: BTreeMap<String, toml::Table>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    light: BTreeMap<String, String>,
    dark: BTreeMap<String, String>,
}

/// The code color names, each the `--hl-<name>` variable in the page.
const COLOR_NAMES: &[&str] =
    &["comment", "keyword", "string", "constant", "entity", "tag", "variable", "inserted", "deleted"];

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
        let config: Self = toml::from_str(text)?;
        for (name, value) in config.colors.light.iter().chain(&config.colors.dark) {
            if !COLOR_NAMES.contains(&name.as_str()) {
                bail!("unknown color `{name}`; the colors are {}", COLOR_NAMES.join(", "));
            }
            // Enough for any CSS color, and not enough to end the rule or the <style> tag.
            if value.is_empty() || !value.chars().all(|c| c.is_ascii_alphanumeric() || "#(),.%/ -".contains(c)) {
                bail!("`{name} = \"{value}\"` isn't a color");
            }
        }
        Ok(config)
    }

    /// CSS that sets the configured code colors, after the page's own.
    pub fn colors_css(&self) -> String {
        let rule = |scope: &str, colors: &BTreeMap<String, String>| {
            let vars: String = colors.iter().map(|(name, value)| format!("  --hl-{name}: {value};\n")).collect();
            format!("{scope} {{\n{vars}}}\n")
        };
        let dark = &self.colors.dark;
        format!(
            "{}@media (prefers-color-scheme: dark) {{\n{}}}\n{}",
            rule(":root", &self.colors.light),
            rule(":root:not([data-theme=\"light\"])", dark),
            rule(":root[data-theme=\"dark\"]", dark),
        )
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

[colors.light]
keyword = "#cf222e"

[colors.dark]
keyword = "rgb(255, 123, 114)"

[plugins.mermaid]
theme = "forest"
"##,
        )
        .unwrap();
        assert!(matches!(config.theme, Some(Theme::Terminal)));
        assert!(matches!(config.split, Some(Direction::Right)));
        assert_eq!(config.watch, Some(false));
        let css = config.colors_css();
        assert!(css.starts_with(":root {\n  --hl-keyword: #cf222e;\n}\n"), "{css}");
        assert!(css.contains(":root[data-theme=\"dark\"] {\n  --hl-keyword: rgb(255, 123, 114);\n}"), "{css}");
        assert_eq!(config.plugins_json(), r#"{"mermaid":{"theme":"forest"}}"#);
    }

    #[test]
    fn an_empty_file_changes_nothing() {
        let config = Config::parse("").unwrap();
        assert!(config.theme.is_none());
        assert!(!config.colors_css().contains("--hl-"));
        assert_eq!(config.plugins_json(), "{}");
    }

    #[test]
    fn rejects_typos_and_values_that_arent_colors() {
        assert!(Config::parse("thme = \"dark\"").is_err());
        assert!(Config::parse("theme = \"blue\"").is_err());
        assert!(Config::parse("[colors.dark]\nkeywrd = \"#fff\"").is_err());
        assert!(Config::parse("[colors.dark]\nkeyword = \"red; } </style>\"").is_err());
    }
}
