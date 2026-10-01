//! Built-in plugins: a pinned library download plus a start-up script kept here.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use super::Loaded;

/// A pinned download. Bump `version`, `url` and `sha256` together.
pub struct Builtin {
    pub name: &'static str,
    pub version: &'static str,
    url: &'static str,
    sha256: &'static str,
    file: &'static str,
    languages: &'static [&'static str],
    init: &'static str,
}

const MERMAID: Builtin = Builtin {
    name: "mermaid",
    version: "12.0.0",
    url: "https://cdn.jsdelivr.net/npm/mermaid@12.0.0/dist/mermaid.min.js",
    sha256: "28fca7ae6ebc7ed7bb63bde63136a74bfef14f296a57e403657eeb8b32836073",
    file: "mermaid.min.js",
    languages: &["mermaid"],
    init: MERMAID_INIT,
};

/// Downloads bigger than this are refused.
const MAX_DOWNLOAD: u64 = 32 * 1024 * 1024;

impl Builtin {
    pub const ALL: &'static [Builtin] = &[MERMAID];

    pub fn named(name: &str) -> Option<&'static Builtin> {
        Self::ALL.iter().find(|b| b.name == name)
    }

    /// `<plugins>/<name>`, holding one folder per version.
    fn root(&self) -> Result<PathBuf> {
        Ok(super::root()?.join(self.name))
    }

    /// Where the pinned version's library lives once installed.
    fn path(&self) -> Result<PathBuf> {
        Ok(self.root()?.join(self.version).join(self.file))
    }

    pub fn is_installed(&self) -> Result<bool> {
        Ok(self.path()?.is_file())
    }

    /// What this plugin would claim, without its library (for conflict checks).
    pub fn claims(&self) -> Loaded {
        Loaded {
            name: self.name.into(),
            languages: self.languages.iter().map(|l| l.to_string()).collect(),
            fallback: false,
            front_matter_keys: Vec::new(),
            scripts: Vec::new(),
            styles: Vec::new(),
        }
    }

    /// The installed plugin, or `None` if the pinned version isn't installed.
    pub fn load(&self) -> Result<Option<Loaded>> {
        let path = self.path()?;
        let library = match fs::read_to_string(&path) {
            Ok(library) => library,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Some(Loaded { scripts: vec![library, self.init.into()], ..self.claims() }))
    }

    /// Download the pinned version with the user's own `curl`, check its SHA-256, and
    /// save it. Returns its path.
    pub fn install(&self) -> Result<PathBuf> {
        let path = self.path()?;
        let out = Command::new("curl")
            .args(["-fsSL", "--max-filesize", &MAX_DOWNLOAD.to_string(), "--", self.url])
            .output()
            .context("running `curl` (is it installed?)")?;
        if !out.status.success() {
            bail!("downloading {}: {}", self.url, String::from_utf8_lossy(&out.stderr).trim());
        }
        let body = out.stdout;
        // Servers that don't send a size get past --max-filesize on older curls.
        if body.len() as u64 > MAX_DOWNLOAD {
            bail!("{} is bigger than {} MB; refusing it", self.url, MAX_DOWNLOAD / 1024 / 1024);
        }
        let digest = hex(&Sha256::digest(&body));
        if digest != self.sha256 {
            bail!("{} has the wrong checksum (expected {}, got {digest}); not installing it", self.url, self.sha256);
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
    pub fn remove(&self) -> Result<bool> {
        let root = self.root()?;
        match fs::remove_dir_all(&root) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err).with_context(|| format!("removing {}", root.display())),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Draw each claimed block, in the Mermaid theme that matches the page's theme. The
/// config file's `[plugins.mermaid]` goes to `mermaid.initialize`, over these defaults.
const MERMAID_INIT: &str = r#"(() => {
  const blocks = [...document.querySelectorAll('pre[data-plugin="mermaid"]')];
  for (const pre of blocks) {
    // Mermaid reads the element's own text, so drop the <code> wrapper and the language label.
    pre.textContent = pre.textContent;
    pre.removeAttribute("data-lang");
  }
  const forced = document.documentElement.dataset.theme;
  const dark = forced ? forced === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
  mermaid.initialize({ startOnLoad: false, theme: dark ? "dark" : "default", ...tmdview.config.mermaid });
  tmdview.ready(mermaid.run({ nodes: blocks }));
})();
"#;
