mod browser;
mod plugins;
mod render;
mod terminal;
mod watch;

use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use render::Renderer;

/// View a markdown file rendered as HTML in a browser inside the terminal.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Markdown file to view. For a file named `plugins`, write `./plugins`.
    #[arg(required = true)]
    file: Option<PathBuf>,

    /// Open in a new pane beside the current one instead of taking over this pane.
    #[arg(short, long, value_enum)]
    split: Option<Direction>,

    /// Fraction of the space the split pane takes (0.2 to 0.95).
    #[arg(long, requires = "split")]
    size: Option<f32>,

    /// Don't reload the page when the file is saved.
    #[arg(long)]
    no_watch: bool,

    /// Check the file for changes by polling, for network drives and other
    /// filesystems that don't report changes.
    #[arg(long)]
    poll: bool,

    /// Internal: run as the background watcher. It reloads the tab showing the page
    /// that isn't one of these comma-separated `<browser>/<tab>`s, open before it was.
    #[arg(long, hide = true, value_name = "TABS", value_delimiter = ',', num_args = 0..=1)]
    attach: Option<Vec<browser::Tab>>,

    /// Color theme. `auto` follows the browser's preference. `terminal` takes the
    /// colors, and in Ghostty the font, from the terminal you run tmdview in.
    #[arg(short, long, value_enum, default_value_t = Theme::Auto)]
    theme: Theme,

    /// Internal: the terminal's style as JSON, from the tmdview that started this
    /// background watcher, which has no terminal to ask.
    #[arg(long, hide = true, requires = "attach")]
    terminal_style: Option<String>,

    /// Write the HTML here instead of a temp file.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Only render the HTML file; don't open a browser.
    #[arg(long)]
    no_open: bool,

    /// Don't use installed plugins; show their code blocks as code.
    #[arg(long)]
    no_plugins: bool,
}

#[derive(Subcommand)]
enum Command {
    /// List, install, update or remove plugins.
    Plugins {
        #[command(subcommand)]
        action: plugins::Action,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Direction {
    Right,
    Left,
    Down,
    Up,
}

#[derive(Clone, Copy, ValueEnum)]
enum Theme {
    Auto,
    Light,
    Dark,
    Terminal,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::Right => "right",
            Direction::Left => "left",
            Direction::Down => "down",
            Direction::Up => "up",
        }
    }
}

impl Theme {
    fn as_str(self) -> &'static str {
        match self {
            Theme::Auto => "auto",
            Theme::Light => "light",
            Theme::Dark => "dark",
            // Resolved to dark or light from the terminal's colors before rendering.
            Theme::Terminal => "auto",
        }
    }
}

struct Page {
    source: PathBuf,
    output: PathBuf,
    /// `auto`, `light` or `dark`.
    scheme: &'static str,
    renderer: Renderer,
}

impl Page {
    /// The Markdown and its hash, so the watcher can skip saves that change nothing.
    fn read(&self) -> Result<(String, u64)> {
        let markdown = fs::read_to_string(&self.source)
            .with_context(|| format!("reading {}", self.source.display()))?;
        let hash = hash_of(&markdown);
        Ok((markdown, hash))
    }

    fn write(&self, markdown: &str) -> Result<()> {
        let title = self.source.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let base_dir = self.source.parent().unwrap_or(Path::new("/"));
        let html = self.renderer.render(markdown, &title, base_dir, self.scheme);
        // Write then rename, so a reload never sees a half-written page.
        let tmp = self.output.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&tmp, html).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &self.output).with_context(|| format!("writing {}", self.output.display()))
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Plugins { action }) = cli.command {
        return plugins::run(action);
    }
    let file = cli.file.as_deref().expect("clap requires a file without a subcommand");
    let source = fs::canonicalize(file).with_context(|| format!("opening {}", file.display()))?;
    let output = match &cli.output {
        Some(path) => path.clone(),
        None => default_output(&source)?,
    };
    let plugins = if cli.no_plugins { Vec::new() } else { plugins::load_installed()? };
    let terminal: Option<terminal::Style> = match (cli.theme, &cli.terminal_style) {
        (Theme::Terminal, Some(json)) => Some(serde_json::from_str(json).context("reading --terminal-style")?),
        // The watcher got no style because detection already failed; don't ask again.
        (Theme::Terminal, None) if cli.attach.is_some() => None,
        (Theme::Terminal, None) => {
            let style = terminal::detect();
            if style.is_none() {
                eprintln!("tmdview: the terminal didn't report its colors; using the auto theme");
            }
            style
        }
        _ => None,
    };
    let page = Page {
        source,
        output,
        scheme: terminal.as_ref().map_or(cli.theme.as_str(), terminal::Style::scheme),
        renderer: Renderer::new(plugins, terminal.as_ref().map(terminal::Style::css))?,
    };

    if let Some(before) = &cli.attach {
        // The tmdview that started us already wrote the page.
        let log = Log::new(watch::log_path(&page.output));
        match browser::find_new_tab(&fs::canonicalize(&page.output)?, before) {
            Some(tab) => watch::run(&page, &tab, cli.poll, &log),
            None => log.say("couldn't find the browser tab; not reloading on save"),
        }
        return Ok(());
    }

    page.write(&page.read()?.0)?;
    // Canonical so it matches the file:// url terminal-browser reports.
    let output = fs::canonicalize(&page.output)?;
    if cli.no_open {
        println!("{}", output.display());
        return Ok(());
    }
    if !cli.no_watch {
        // Started before the browser opens: it finds the new tab itself, which can take
        // seconds while a browser starts, so nothing here waits for it.
        spawn_watcher(&browser::snapshot(), terminal.as_ref())?;
    }
    match cli.split {
        Some(direction) => {
            browser::open_split(&output, direction.as_str(), cli.size)?;
            if !cli.no_watch {
                eprintln!("tmdview: reloading on every save; messages go to {}", watch::log_path(&page.output).display());
            }
        }
        None => {
            browser::open_here(&output)?.wait()?;
        }
    }
    Ok(())
}

/// Start a detached copy of tmdview, with the same arguments plus `--attach`, that
/// reloads the new tab on every save and exits when the tab closes. It runs in its own
/// process group, so Ctrl-C in the shell doesn't stop it.
fn spawn_watcher(before: &[browser::Tab], terminal: Option<&terminal::Style>) -> Result<()> {
    let mut cmd = Process::new(std::env::current_exe().context("finding the tmdview binary")?);
    cmd.args(std::env::args_os().skip(1));
    if let Some(style) = terminal {
        cmd.arg("--terminal-style").arg(serde_json::to_string(style)?);
    }
    cmd.arg("--attach");
    if !before.is_empty() {
        cmd.arg(before.iter().map(ToString::to_string).collect::<Vec<_>>().join(","));
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.spawn().context("starting the background watcher")?;
    Ok(())
}

/// The background watcher's messages. It has no terminal, so they go to a file,
/// started fresh each run.
struct Log {
    path: PathBuf,
}

impl Log {
    fn new(path: PathBuf) -> Self {
        let _ = fs::write(&path, "");
        Self { path }
    }

    fn say(&self, msg: impl std::fmt::Display) {
        let file = fs::OpenOptions::new().create(true).append(true).open(&self.path);
        if let Ok(mut file) = file {
            let _ = writeln!(file, "tmdview: {msg}");
        }
    }
}

fn hash_of(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// A stable per-source temp path, so re-running tmdview on a file reuses its tab url.
fn default_output(source: &Path) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("tmdview");
    fs::create_dir_all(&dir)?;
    let stem = source.file_stem().map_or_else(|| "page".into(), |s| s.to_string_lossy());
    Ok(dir.join(format!("{stem}-{:016x}.html", hash_of(source))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_takes_no_tabs_or_a_comma_list() {
        let parse = |args: &[&str]| Cli::try_parse_from(["tmdview", "notes.md"].iter().chain(args)).unwrap().attach;
        assert!(parse(&[]).is_none());
        assert_eq!(parse(&["--attach"]), Some(Vec::new()));
        let tabs = parse(&["--attach", "123-1/2,9-1/10"]).unwrap();
        assert_eq!(tabs.iter().map(ToString::to_string).collect::<Vec<_>>(), ["123-1/2", "9-1/10"]);
        assert!(Cli::try_parse_from(["tmdview", "notes.md", "--attach", "nope"]).is_err());
    }
}
