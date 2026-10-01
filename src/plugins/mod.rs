//! Plugins: client-side scripts and styles inlined into pages that have a code
//! block they claim. Built-in plugins are pinned downloads (`builtin.rs`); any
//! other plugin is a git repository with a manifest (`git.rs`). Both live in
//! `<data folder>/tmdview/plugins/<name>/`.

mod builtin;
mod git;

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Subcommand;

use builtin::Builtin;

#[derive(Subcommand)]
pub enum Action {
    /// Show built-in plugins and every installed plugin.
    List,
    /// Install a built-in plugin by name, or a plugin from a git repository.
    ///
    /// SOURCE is a built-in name (`mermaid`), a git URL, a local repository
    /// path, or GitHub shorthand (`owner/repo`). Installed plugins are used automatically.
    Install {
        #[arg(required = true)]
        sources: Vec<String>,
        /// Branch, tag or commit to install from a git repository.
        #[arg(long = "ref", value_name = "REF")]
        git_ref: Option<String>,
        /// Don't ask before installing code from a git repository.
        #[arg(short, long)]
        yes: bool,
    },
    /// Update git plugins to the newest commit of their branch or ref.
    Update {
        /// Plugins to update; all git plugins if none are given.
        names: Vec<String>,
        /// Don't ask before switching to the new code.
        #[arg(short, long)]
        yes: bool,
    },
    /// Delete installed plugins.
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
}

/// An installed plugin, read into memory and ready to inline.
pub struct Loaded {
    pub name: String,
    /// Fenced code block languages it takes over, even ones tmdview can highlight.
    pub languages: Vec<String>,
    /// Also takes blocks whose language nothing else handles.
    pub fallback: bool,
    /// Also runs on pages whose front matter has any of these top-level keys.
    pub front_matter_keys: Vec<String>,
    pub scripts: Vec<String>,
    pub styles: Vec<String>,
}

impl Loaded {
    pub fn claims(&self, lang: &str) -> bool {
        self.languages.iter().any(|l| l == lang)
    }

    /// Whether a page with front matter keys `keys` needs this plugin.
    pub fn wants_front_matter(&self, keys: &[String]) -> bool {
        self.front_matter_keys.iter().any(|k| keys.contains(k))
    }

    /// The `<style>` and `<script>` tags added to a page that uses this plugin.
    pub fn assets(&self) -> String {
        let mut out = String::new();
        for css in &self.styles {
            out.push_str(&format!("<style>\n{}\n</style>\n", neutralize(css, "</style")));
        }
        for js in &self.scripts {
            out.push_str(&script_tag(js));
        }
        out
    }
}

/// A `<script>` that runs `js` exactly as written.
///
/// Scripts are inlined, with `</script` broken up so it can't end the tag. If a script
/// also has `<!--` and `<script`, the HTML parser can enter its "double-escaped" state,
/// where even the real `</script>` no longer ends the tag. Rewriting those in the code
/// could break it (`\!` is a syntax error in a `u`-flag regex), so that rare script goes
/// in as a percent-encoded `data:` URL instead, which contains no markup at all.
fn script_tag(js: &str) -> String {
    let lower = js.to_ascii_lowercase();
    if lower.contains("<!--") && lower.contains("<script") {
        let url = crate::render::percent_encode(js.as_bytes());
        return format!("<script src=\"data:text/javascript;charset=utf-8,{url}\"></script>\n");
    }
    format!("<script>\n{}\n</script>\n", neutralize(js, "</script"))
}

/// Break up `tag` (any case) so inlined code can't close the element it's in.
fn neutralize(code: &str, tag: &str) -> String {
    let lower = code.to_ascii_lowercase();
    let mut out = String::with_capacity(code.len());
    let mut last = 0;
    for (i, _) in lower.match_indices(tag) {
        out.push_str(&code[last..i + 1]);
        out.push('\\');
        last = i + 1;
    }
    out.push_str(&code[last..]);
    out
}

/// `<data folder>/tmdview/plugins`.
fn root() -> Result<PathBuf> {
    Ok(data_dir()?.join("tmdview").join("plugins"))
}

/// The OS's folder for app data: `~/Library/Application Support` on macOS,
/// `$XDG_DATA_HOME` or `~/.local/share` elsewhere.
fn data_dir() -> Result<PathBuf> {
    let home = || std::env::var_os("HOME").map(PathBuf::from).context("couldn't find your data folder: HOME isn't set");
    if cfg!(target_os = "macos") {
        return Ok(home()?.join("Library/Application Support"));
    }
    match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir),
        _ => Ok(home()?.join(".local/share")),
    }
}

/// Every installed plugin: built-ins first, then git plugins by name. When two
/// claim the same language, the first one wins. A git plugin that fails to load
/// is skipped with a warning, so one broken plugin doesn't stop the page.
pub fn load_installed() -> Result<Vec<Loaded>> {
    let mut loaded = Vec::new();
    for builtin in Builtin::ALL {
        loaded.extend(builtin.load()?);
    }
    for name in git::installed()? {
        match git::load(&name) {
            Ok(plugin) => loaded.push(plugin),
            Err(err) => eprintln!("tmdview: skipping plugin {name}: {err:#}"),
        }
    }
    Ok(loaded)
}

pub fn run(action: Action) -> Result<()> {
    match action {
        Action::List => list(),
        Action::Install { sources, git_ref, yes } => {
            for source in sources {
                match Builtin::named(&source) {
                    Some(builtin) => {
                        if git_ref.is_some() {
                            bail!("--ref only applies to git plugins, not {source}");
                        }
                        check_conflicts(&builtin.claims(), &load_installed()?)?;
                        eprintln!("tmdview: downloading {} {}", builtin.name, builtin.version);
                        let path = builtin.install()?;
                        eprintln!("tmdview: installed {}", path.display());
                    }
                    None => git::install(&source, git_ref.as_deref(), yes)?,
                }
            }
            Ok(())
        }
        Action::Update { names, yes } => {
            let names = if names.is_empty() { git::installed()? } else { names };
            if names.is_empty() {
                eprintln!("tmdview: no git plugins installed");
            }
            for name in names {
                if Builtin::named(&name).is_some() {
                    bail!("{name} is built in; it updates with tmdview itself");
                }
                git::update(&name, yes)?;
            }
            Ok(())
        }
        Action::Remove { names } => {
            for name in names {
                let removed = match Builtin::named(&name) {
                    Some(builtin) => builtin.remove()?,
                    None => git::remove(&name)?,
                };
                let what = if removed { "removed" } else { "wasn't installed:" };
                eprintln!("tmdview: {what} {name}");
            }
            Ok(())
        }
    }
}

fn list() -> Result<()> {
    for builtin in Builtin::ALL {
        let status = if builtin.is_installed()? { "installed" } else { "not installed" };
        println!("{:<12} {:<9} built-in  {status}", builtin.name, builtin.version);
    }
    for name in git::installed()? {
        match git::describe(&name) {
            Ok(line) => println!("{line}"),
            Err(err) => println!("{name:<12} {:<9} git       broken: {err:#}", "?"),
        }
    }
    Ok(())
}

/// Fail if `new` would claim a language, or the fallback, that another installed plugin has.
fn check_conflicts(new: &Loaded, installed: &[Loaded]) -> Result<()> {
    for other in installed.iter().filter(|o| o.name != new.name) {
        if let Some(lang) = new.languages.iter().find(|l| other.claims(l)) {
            bail!("{} and {} both claim ```{lang} blocks; remove {} first", new.name, other.name, other.name);
        }
        if new.fallback && other.fallback {
            bail!("{} and {} are both fallback plugins; remove {} first", new.name, other.name, other.name);
        }
    }
    Ok(())
}

/// Ask a yes/no question on the terminal. `yes` answers it up front.
fn confirm(question: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        bail!("can't ask for confirmation without a terminal; pass --yes");
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inlined_code_cannot_close_its_tag() {
        assert_eq!(neutralize(r#"x = "</script>"; y = "</SCRIPT""#, "</script"), r#"x = "<\/script>"; y = "<\/SCRIPT""#);
    }

    #[test]
    fn scripts_that_could_swallow_the_page_go_in_as_data_urls() {
        let risky = r#"const a = "<!--"; const b = "<script>";"#;
        let tag = script_tag(risky);
        assert!(tag.starts_with(r#"<script src="data:text/javascript;charset=utf-8,const%20a"#), "{tag}");
        assert!(!tag.contains("<!--"), "{tag}");
        assert_eq!(script_tag("let x = 1;"), "<script>\nlet x = 1;\n</script>\n");
    }

    #[test]
    fn conflicting_languages_are_rejected() {
        let plugin = |name: &str, langs: &[&str], fallback| Loaded {
            name: name.into(),
            languages: langs.iter().map(|l| l.to_string()).collect(),
            fallback,
            front_matter_keys: Vec::new(),
            scripts: Vec::new(),
            styles: Vec::new(),
        };
        let installed = [plugin("a", &["d2"], true)];
        assert!(check_conflicts(&plugin("b", &["d2"], false), &installed).is_err());
        assert!(check_conflicts(&plugin("b", &[], true), &installed).is_err());
        assert!(check_conflicts(&plugin("b", &["dot"], false), &installed).is_ok());
        assert!(check_conflicts(&plugin("a", &["d2"], true), &installed).is_ok(), "a plugin never conflicts with itself");
    }
}
