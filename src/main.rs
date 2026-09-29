mod browser;
mod plugins;
mod render;

use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

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

    /// Re-render and reload the page whenever the file changes.
    #[arg(short, long)]
    watch: bool,

    /// Color theme. `auto` follows the browser's preference.
    #[arg(short, long, value_enum, default_value_t = Theme::Auto)]
    theme: Theme,

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
        }
    }
}

struct Page {
    source: PathBuf,
    output: PathBuf,
    theme: Theme,
    renderer: Renderer,
}

impl Page {
    fn write(&self) -> Result<()> {
        let markdown = fs::read_to_string(&self.source)
            .with_context(|| format!("reading {}", self.source.display()))?;
        let title = self.source.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let base_dir = self.source.parent().unwrap_or(Path::new("/"));
        let html = self.renderer.render(&markdown, &title, base_dir, self.theme.as_str());
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
    let file = cli.file.expect("clap requires a file without a subcommand");
    let source = fs::canonicalize(&file).with_context(|| format!("opening {}", file.display()))?;
    let output = match &cli.output {
        Some(path) => path.clone(),
        None => default_output(&source)?,
    };

    let plugins = if cli.no_plugins { Vec::new() } else { plugins::load_installed()? };
    let page = Page { source, output, theme: cli.theme, renderer: Renderer::new(plugins)? };
    page.write()?;
    // Canonical so it matches the file:// url terminal-browser reports.
    let output = fs::canonicalize(&page.output)?;

    if cli.no_open {
        println!("{}", output.display());
        return Ok(());
    }

    let before = browser::snapshot();
    let child = match cli.split {
        Some(direction) => {
            browser::open_split(&output, direction.as_str(), cli.size)?;
            None
        }
        None => Some(browser::open_here(&output)?),
    };
    if !cli.watch {
        if let Some(mut child) = child {
            child.wait()?;
        }
        return Ok(());
    }

    // While the browser owns this terminal, anything we print would land on top of
    // it, so messages go to a log file until the browser process exits.
    let log = Log::new(page.output.with_extension("log"), child.is_some());
    let waiter = child.map(|mut child| {
        let log = log.clone();
        thread::spawn(move || {
            child.wait().ok();
            log.release_terminal();
        })
    });
    watch(&page, &output, &before, &log);
    if let Some(waiter) = waiter {
        waiter.join().ok();
    }
    Ok(())
}

/// Where watch messages go: stderr, or a log file while the browser owns the terminal.
#[derive(Clone)]
struct Log {
    path: PathBuf,
    terminal_owned: Arc<AtomicBool>,
}

impl Log {
    fn new(path: PathBuf, terminal_owned: bool) -> Self {
        Self { path, terminal_owned: Arc::new(AtomicBool::new(terminal_owned)) }
    }

    fn release_terminal(&self) {
        self.terminal_owned.store(false, Ordering::Relaxed);
    }

    fn say(&self, msg: impl std::fmt::Display) {
        if !self.terminal_owned.load(Ordering::Relaxed) {
            eprintln!("tmdview: {msg}");
            return;
        }
        let file = fs::OpenOptions::new().create(true).append(true).open(&self.path);
        if let Ok(mut file) = file {
            let _ = writeln!(file, "tmdview: {msg}");
        }
    }
}

/// A stable per-source temp path, so re-running tmdview on a file reuses its tab url.
fn default_output(source: &Path) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("tmdview");
    fs::create_dir_all(&dir)?;
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    let stem = source.file_stem().map_or_else(|| "page".into(), |s| s.to_string_lossy());
    Ok(dir.join(format!("{stem}-{:016x}.html", hasher.finish())))
}

/// Poll the source file and reload the tab on change, until the tab is closed.
fn watch(page: &Page, output: &Path, before: &[browser::Tab], log: &Log) {
    let Some(tab) = browser::find_new_tab(output, before) else {
        log.say("couldn't find the browser tab; not watching");
        return;
    };
    log.say(format_args!("watching {} (Ctrl-C to stop)", page.source.display()));
    let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut last: Option<SystemTime> = mtime(&page.source);

    for tick in 1u64.. {
        thread::sleep(Duration::from_millis(250));
        // Every 2s, stop if the user closed the tab or quit the browser.
        if tick % 8 == 0 && !browser::is_open(&tab) {
            return;
        }
        let now = mtime(&page.source);
        // Editors often replace the file (brief absence); wait until it's back.
        if now.is_none() || now == last {
            continue;
        }
        last = now;
        if let Err(err) = page.write() {
            log.say(format_args!("{err:#}"));
            continue;
        }
        if !browser::reload(&tab) {
            if !browser::is_open(&tab) {
                log.say("browser tab closed; stopping");
                return;
            }
            log.say("couldn't reload the browser tab; will retry on the next change");
        }
    }
}
