//! Built-in plugins: a pinned library download plus a start-up script kept here.

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use super::Loaded;

#[derive(Clone, Copy)]
pub enum Builtin {
    Mermaid,
}

/// A pinned download. Bump `version`, `url` and `sha256` together.
struct Spec {
    name: &'static str,
    version: &'static str,
    url: &'static str,
    sha256: &'static str,
    file: &'static str,
    languages: &'static [&'static str],
    init: &'static str,
}

const MERMAID: Spec = Spec {
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
    pub const ALL: [Builtin; 1] = [Builtin::Mermaid];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }

    fn spec(self) -> &'static Spec {
        match self {
            Builtin::Mermaid => &MERMAID,
        }
    }

    pub fn name(self) -> &'static str {
        self.spec().name
    }

    pub fn version(self) -> &'static str {
        self.spec().version
    }

    /// `<plugins>/<name>`, holding one folder per version.
    fn root(self) -> Result<PathBuf> {
        Ok(super::root()?.join(self.name()))
    }

    /// Where the pinned version's library lives once installed.
    fn path(self) -> Result<PathBuf> {
        Ok(self.root()?.join(self.version()).join(self.spec().file))
    }

    pub fn is_installed(self) -> Result<bool> {
        Ok(self.path()?.is_file())
    }

    /// What this plugin would claim, without its library (for conflict checks).
    pub fn claims(self) -> Loaded {
        let spec = self.spec();
        Loaded {
            name: spec.name.into(),
            languages: spec.languages.iter().map(|l| l.to_string()).collect(),
            fallback: false,
            front_matter_keys: Vec::new(),
            scripts: Vec::new(),
            styles: Vec::new(),
        }
    }

    /// The installed plugin, or `None` if the pinned version isn't installed.
    pub fn load(self) -> Result<Option<Loaded>> {
        let path = self.path()?;
        let library = match fs::read_to_string(&path) {
            Ok(library) => library,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Some(Loaded { scripts: vec![library, self.spec().init.into()], ..self.claims() }))
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Draw each claimed block, in the Mermaid theme that matches the page's theme.
const MERMAID_INIT: &str = r#"(() => {
  const blocks = [...document.querySelectorAll('pre[data-plugin="mermaid"]')];
  for (const pre of blocks) {
    // Mermaid reads the element's own text, so drop the <code> wrapper and the language label.
    pre.textContent = pre.textContent;
    pre.removeAttribute("data-lang");
  }
  const forced = document.documentElement.dataset.theme;
  const dark = forced ? forced === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
  mermaid.initialize({ startOnLoad: false, theme: dark ? "dark" : "default" });
  tmdview.ready(mermaid.run({ nodes: blocks }));
})();
"#;
