//! Git plugins: a repository with a `tmdview-plugin.toml` manifest, cloned with
//! the user's own `git` so private repositories work with their usual credentials.
//!
//! Layout: `<plugins>/<name>/source.json` (where it came from) and `<plugins>/<name>/repo/`.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::builtin::Builtin;
use super::{check_conflicts, confirm, load_installed, Loaded};

pub const MANIFEST: &str = "tmdview-plugin.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    name: String,
    version: String,
    description: Option<String>,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    fallback: bool,
    #[serde(default)]
    scripts: Vec<String>,
    #[serde(default)]
    styles: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Source {
    url: String,
    /// The `--ref` it was installed with; `None` follows the default branch.
    git_ref: Option<String>,
}

fn dir(name: &str) -> Result<PathBuf> {
    Ok(super::root()?.join(name))
}

/// Names of installed git plugins, sorted.
pub fn installed() -> Result<Vec<String>> {
    let root = super::root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", root.display())),
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("source.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    Ok(names)
}

pub fn load(name: &str) -> Result<Loaded> {
    let manifest = read_manifest(&dir(name)?.join("repo"))?;
    if manifest.name != name {
        bail!("its manifest now says it's called {}; reinstall it", manifest.name);
    }
    load_repo(&dir(name)?.join("repo"), manifest)
}

/// One line for `plugins list`.
pub fn describe(name: &str) -> Result<String> {
    let dir = dir(name)?;
    let manifest = read_manifest(&dir.join("repo"))?;
    let source = read_source(&dir)?;
    let commit = git(&dir.join("repo"), &["rev-parse", "--short", "HEAD"])?;
    let pin = source.git_ref.map_or_else(String::new, |r| format!(" (ref {r})"));
    Ok(format!("{name:<12} {:<9} git       {} @ {commit}{pin}", manifest.version, source.url))
}

pub fn install(source: &str, git_ref: Option<&str>, yes: bool) -> Result<()> {
    let url = resolve_url(source)?;
    let root = super::root()?;
    fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let staging = Staging(root.join(format!(".install-{}", std::process::id())));
    let _ = fs::remove_dir_all(&staging.0);
    let repo = staging.0.join("repo");

    eprintln!("tmdview: cloning {url}");
    run_git(Command::new("git").args(["clone", "--quiet", "--", &url]).arg(&repo))
        .with_context(|| format!("cloning {url}"))?;
    if let Some(r) = git_ref {
        let commit = resolve_ref(&repo, r)?;
        git(&repo, &["checkout", "--quiet", "--detach", &commit])?;
    }

    let manifest = read_manifest(&repo)?;
    let name = manifest.name.clone();
    if dir(&name)?.exists() {
        bail!("{name} is already installed; run `tmdview plugins update {name}` or remove it first");
    }
    let summary = summarize(&manifest, &url, &git(&repo, &["rev-parse", "--short", "HEAD"])?);
    let plugin = load_repo(&repo, manifest)?;
    check_conflicts(&plugin, &load_installed()?)?;

    eprintln!("{summary}");
    eprintln!("It runs its own JavaScript in every page that uses it.");
    if !confirm(&format!("Install {name}?"), yes)? {
        eprintln!("tmdview: not installed");
        return Ok(());
    }
    let source = Source { url, git_ref: git_ref.map(String::from) };
    fs::write(staging.0.join("source.json"), serde_json::to_string_pretty(&source)?)?;
    fs::rename(&staging.0, dir(&name)?).with_context(|| format!("installing {name}"))?;
    eprintln!("tmdview: installed {name}");
    Ok(())
}

pub fn update(name: &str, yes: bool) -> Result<()> {
    validate_name(name)?;
    let dir = dir(name)?;
    if !dir.join("source.json").is_file() {
        bail!("{name} isn't installed");
    }
    let source = read_source(&dir)?;
    let repo = dir.join("repo");
    let old = git(&repo, &["rev-parse", "HEAD"])?;

    run_git(Command::new("git").arg("-C").arg(&repo).args(["fetch", "--quiet", "--tags", "--force", "origin"]))
        .with_context(|| format!("fetching {}", source.url))?;
    let new = match &source.git_ref {
        Some(r) => resolve_ref(&repo, r)?,
        None => git(&repo, &["rev-parse", "--verify", "origin/HEAD^{commit}"])?,
    };
    if new == old {
        eprintln!("tmdview: {name} is up to date ({})", &old[..7]);
        return Ok(());
    }

    git(&repo, &["checkout", "--quiet", "--detach", &new])?;
    let checked = (|| -> Result<String> {
        let manifest = read_manifest(&repo)?;
        if manifest.name != name {
            bail!("the new commit renames the plugin to {}; reinstall it instead", manifest.name);
        }
        let summary = summarize(&manifest, &source.url, &new[..7]);
        check_conflicts(&load_repo(&repo, manifest)?, &load_installed()?)?;
        Ok(summary)
    })();
    let accepted = checked.and_then(|summary| {
        eprintln!("{summary}");
        eprintln!("Updating {name} from {} to {}.", &old[..7], &new[..7]);
        confirm(&format!("Update {name}?"), yes)
    });
    if let Ok(true) = accepted {
        eprintln!("tmdview: updated {name}");
        return Ok(());
    }
    // Declined or broken: go back to the commit that worked.
    git(&repo, &["checkout", "--quiet", "--detach", &old])?;
    accepted.with_context(|| format!("kept {name} at {}", &old[..7]))?;
    eprintln!("tmdview: kept {name} at {}", &old[..7]);
    Ok(())
}

pub fn remove(name: &str) -> Result<bool> {
    validate_name(name)?;
    let dir = dir(name)?;
    if !dir.join("source.json").is_file() {
        return Ok(false);
    }
    fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    Ok(true)
}

fn read_source(dir: &Path) -> Result<Source> {
    let text = fs::read_to_string(dir.join("source.json")).context("reading source.json")?;
    serde_json::from_str(&text).context("reading source.json")
}

/// Read a repo's manifest and check what it declares.
fn read_manifest(repo: &Path) -> Result<Manifest> {
    let path = repo.join(MANIFEST);
    let text = fs::read_to_string(&path).with_context(|| format!("no {MANIFEST} in the repository"))?;
    let manifest: Manifest = toml::from_str(&text).with_context(|| format!("reading {MANIFEST}"))?;
    validate_name(&manifest.name)?;
    if Builtin::named(&manifest.name).is_some() {
        bail!("{MANIFEST}: {} is a built-in plugin's name", manifest.name);
    }
    if manifest.languages.is_empty() && !manifest.fallback {
        bail!("{MANIFEST}: set `languages`, `fallback = true`, or both");
    }
    if let Some(lang) = manifest.languages.iter().find(|l| l.is_empty() || l.contains(char::is_whitespace)) {
        bail!("{MANIFEST}: {lang:?} isn't a code block language");
    }
    if manifest.scripts.is_empty() && manifest.styles.is_empty() {
        bail!("{MANIFEST}: list at least one file in `scripts` or `styles`");
    }
    Ok(manifest)
}

/// Read the files a manifest lists. They must stay inside the repository.
fn load_repo(repo: &Path, manifest: Manifest) -> Result<Loaded> {
    let read = |files: &[String]| -> Result<Vec<String>> {
        files.iter().map(|f| read_inside(repo, f)).collect()
    };
    Ok(Loaded {
        scripts: read(&manifest.scripts)?,
        styles: read(&manifest.styles)?,
        name: manifest.name,
        languages: manifest.languages,
        fallback: manifest.fallback,
    })
}

fn read_inside(repo: &Path, file: &str) -> Result<String> {
    let rel = Path::new(file);
    if !rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        bail!("{MANIFEST}: {file:?} must be a path inside the repository");
    }
    let path = repo.join(rel);
    // A symlink could still point outside, so compare the resolved paths too.
    let real = fs::canonicalize(&path).with_context(|| format!("{MANIFEST} lists {file}, which doesn't exist"))?;
    if !real.starts_with(fs::canonicalize(repo)?) {
        bail!("{MANIFEST}: {file:?} points outside the repository");
    }
    fs::read_to_string(&real).with_context(|| format!("reading {file}"))
}

fn summarize(manifest: &Manifest, url: &str, commit: &str) -> String {
    let mut claims: Vec<String> = manifest.languages.iter().map(|l| format!("```{l}")).collect();
    if manifest.fallback {
        claims.push("any language nothing else handles".into());
    }
    let mut out = format!("{} {} from {url} @ {commit}\n", manifest.name, manifest.version);
    if let Some(description) = &manifest.description {
        out.push_str(&format!("  {description}\n"));
    }
    out.push_str(&format!("  Takes over: {}", claims.join(", ")));
    out
}

fn validate_name(name: &str) -> Result<()> {
    let ok = name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        bail!("{name:?} isn't a valid plugin name (lowercase letters, digits and dashes)");
    }
    Ok(())
}

/// Accept `owner/repo` as GitHub shorthand, and make local paths absolute so
/// `update` still finds them later. Anything else goes to git as it is.
fn resolve_url(source: &str) -> Result<String> {
    let path = Path::new(source);
    if path.is_dir() {
        return Ok(fs::canonicalize(path)?.to_string_lossy().into_owned());
    }
    if is_github_shorthand(source) {
        return Ok(format!("https://github.com/{source}"));
    }
    Ok(source.to_string())
}

fn is_github_shorthand(source: &str) -> bool {
    let valid = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    match source.split_once('/') {
        Some((owner, repo)) => valid(owner) && valid(repo) && !owner.starts_with('.'),
        None => false,
    }
}

/// A commit for `r`: a remote branch first (so updates follow it), then a tag or commit.
fn resolve_ref(repo: &Path, r: &str) -> Result<String> {
    for candidate in [format!("origin/{r}^{{commit}}"), format!("{r}^{{commit}}")] {
        if let Ok(commit) = git(repo, &["rev-parse", "--verify", "--quiet", &candidate]) {
            return Ok(commit);
        }
    }
    bail!("{r} isn't a branch, tag or commit in the repository")
}

/// Run git in `repo` and return its trimmed stdout.
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .context("running `git` (is it installed?)")?;
    if !out.status.success() {
        bail!("`git {}` failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run a git command that may talk to a server. Its stderr goes to the terminal,
/// so the user sees credential prompts and errors as git prints them.
fn run_git(cmd: &mut Command) -> Result<()> {
    let status = cmd.stdout(Stdio::null()).status().context("running `git` (is it installed?)")?;
    if !status.success() {
        bail!("git exited with {status}");
    }
    Ok(())
}

/// A half-finished install, deleted unless it was renamed into place.
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_shorthand() {
        assert!(is_github_shorthand("slara/tmdview-highlight"));
        assert!(!is_github_shorthand("git@github.com:slara/x.git"));
        assert!(!is_github_shorthand("https://github.com/slara/x"));
        assert!(!is_github_shorthand("../x"));
        assert!(!is_github_shorthand("a/b/c"));
    }

    #[test]
    fn plugin_names() {
        assert!(validate_name("d2-diagrams").is_ok());
        assert!(validate_name("../etc").is_err());
        assert!(validate_name("-x").is_err());
        assert!(validate_name("Foo").is_err());
    }

    #[test]
    fn manifest_files_must_stay_in_the_repo() {
        let repo = std::env::temp_dir().join(format!("tmdview-test-{}", std::process::id()));
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("a.js"), "ok").unwrap();
        assert_eq!(read_inside(&repo, "a.js").unwrap(), "ok");
        assert!(read_inside(&repo, "../a.js").is_err());
        assert!(read_inside(&repo, "/etc/hosts").is_err());
        fs::remove_dir_all(&repo).unwrap();
    }
}
