//! Plugins: client-side libraries that `tmdview plugins install` downloads into the
//! user's data folder. An installed plugin is inlined into pages that have a code
//! block it claims.

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use sha2::{Digest, Sha256};

use crate::render::escape;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Plugin {
    /// Draw ```mermaid blocks as diagrams.
    Mermaid,
}

/// A pinned download. Bump `version`, `url` and `sha256` together.
struct Spec {
    name: &'static str,
    version: &'static str,
    url: &'static str,
    sha256: &'static str,
    file: &'static str,
}

const MERMAID: Spec = Spec {
    name: "mermaid",
    version: "12.0.0",
    url: "https://cdn.jsdelivr.net/npm/mermaid@12.0.0/dist/mermaid.min.js",
    sha256: "28fca7ae6ebc7ed7bb63bde63136a74bfef14f296a57e403657eeb8b32836073",
    file: "mermaid.min.js",
};

/// Downloads bigger than this are refused.
const MAX_DOWNLOAD: u64 = 32 * 1024 * 1024;

/// An installed plugin with its library read into memory.
pub struct Loaded {
    pub plugin: Plugin,
    pub library: String,
}

impl Plugin {
    fn spec(self) -> &'static Spec {
        match self {
            Plugin::Mermaid => &MERMAID,
        }
    }

    pub fn name(self) -> &'static str {
        self.spec().name
    }

    pub fn version(self) -> &'static str {
        self.spec().version
    }

    /// Whether this plugin takes over fenced code blocks tagged `lang`.
    pub fn claims(self, lang: &str) -> bool {
        match self {
            Plugin::Mermaid => lang == "mermaid",
        }
    }

    /// HTML for a claimed code block, in place of the highlighted `<pre>`.
    pub fn block(self, src: &str) -> String {
        match self {
            Plugin::Mermaid => format!("<pre class=\"mermaid\">{}</pre>\n", escape(src)),
        }
    }

    /// Scripts appended to the page when at least one block was claimed.
    pub fn scripts(self, library: &str) -> String {
        let init = match self {
            Plugin::Mermaid => MERMAID_INIT,
        };
        format!("<script>\n{library}\n</script>\n<script>\n{init}</script>\n")
    }

    /// `<data dir>/tmdview/plugins/<name>`, holding one folder per version.
    fn root(self) -> Result<PathBuf> {
        let data = dirs::data_dir().context("couldn't find your data folder")?;
        Ok(data.join("tmdview").join("plugins").join(self.name()))
    }

    /// Where the pinned version's library lives once installed.
    pub fn path(self) -> Result<PathBuf> {
        Ok(self.root()?.join(self.version()).join(self.spec().file))
    }

    /// The installed library, or `None` if the pinned version isn't installed.
    pub fn load(self) -> Result<Option<Loaded>> {
        let path = self.path()?;
        match fs::read_to_string(&path) {
            Ok(library) => Ok(Some(Loaded { plugin: self, library })),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Download the pinned version, check its SHA-256, and save it. Returns its path.
    pub fn install(self) -> Result<PathBuf> {
        let spec = self.spec();
        let path = self.path()?;
        let mut body = Vec::new();
        ureq::get(spec.url)
            .call()
            .with_context(|| format!("downloading {}", spec.url))?
            .into_body()
            .into_reader()
            .take(MAX_DOWNLOAD + 1)
            .read_to_end(&mut body)
            .with_context(|| format!("downloading {}", spec.url))?;
        if body.len() as u64 > MAX_DOWNLOAD {
            bail!("{} is bigger than {} MB; refusing it", spec.url, MAX_DOWNLOAD / 1024 / 1024);
        }
        let digest = hex(&Sha256::digest(&body));
        if digest != spec.sha256 {
            bail!("{} has the wrong checksum (expected {}, got {digest}); not installing it", spec.url, spec.sha256);
        }
        let dir = path.parent().expect("plugin path has a parent");
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        // Write then rename, so a failed install never leaves a partial library behind.
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&tmp, &body).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    /// Delete every installed version. Returns false if nothing was installed.
    pub fn remove(self) -> Result<bool> {
        let root = self.root()?;
        match fs::remove_dir_all(&root) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err).with_context(|| format!("removing {}", root.display())),
        }
    }
}

/// Every installed plugin, loaded and ready to inline.
pub fn load_installed() -> Result<Vec<Loaded>> {
    let mut loaded = Vec::new();
    for plugin in Plugin::value_variants() {
        loaded.extend(plugin.load()?);
    }
    Ok(loaded)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Pick the Mermaid theme that matches the page's (forced or system) theme.
const MERMAID_INIT: &str = r#"(() => {
  const forced = document.documentElement.dataset.theme;
  const dark = forced ? forced === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
  mermaid.initialize({ startOnLoad: false, theme: dark ? "dark" : "default" });
  window.tmdviewReady = mermaid.run({ querySelector: "pre.mermaid" });
})();
"#;
