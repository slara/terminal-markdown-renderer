//! Rebuild the page and reload its tab when the Markdown file is saved.
//!
//! Changes come from the OS (inotify on Linux, FSEvents on macOS) through the
//! `notify` crate, with a polling fallback for filesystems that don't send events.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};

use crate::browser::{self, Tab};
use crate::{Log, Page};

/// How long a save's burst of events must go quiet before the page is rebuilt.
const SETTLE: Duration = Duration::from_millis(75);
/// How often to check that the tab is still open when nothing changes.
const TAB_CHECK: Duration = Duration::from_secs(5);
/// How often the polling fallback checks the file.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// A stream of "the source file may have changed" signals.
struct Changes {
    rx: Receiver<()>,
    // Kept alive for as long as we want events.
    _watcher: Box<dyn Watcher + Send>,
    mode: &'static str,
}

enum Wait {
    Changed,
    Idle,
    Stopped,
}

impl Changes {
    /// Watch `source` with OS events, or by polling if `poll` is set or events aren't available.
    fn new(source: &Path, poll: bool, log: &Log) -> Result<Self> {
        if !poll {
            match Self::events(source) {
                Ok(changes) => return Ok(changes),
                Err(err) => log.say(format_args!("file events unavailable ({err:#}); polling instead")),
            }
        }
        Self::polling(source)
    }

    /// Watch the file's folder, not the file. Editors that save by writing a temp file
    /// and renaming it over the original replace the file, and a watch on the file
    /// itself would stop after the first such save.
    fn events(source: &Path) -> Result<Self> {
        let dir = source.parent().context("the file has no folder")?.to_path_buf();
        let name: OsString = source.file_name().context("the file has no name")?.to_owned();
        let (tx, rx) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res
                    && is_change(&event.kind)
                    && event.paths.iter().any(|p| p.file_name() == Some(&*name))
                {
                    let _ = tx.send(());
                }
            },
            notify::Config::default(),
        )?;
        watcher.watch(&dir, RecursiveMode::NonRecursive).with_context(|| format!("watching {}", dir.display()))?;
        Ok(Self { rx, _watcher: Box::new(watcher), mode: "file events" })
    }

    /// Poll the file itself, comparing contents so saves within the same second still count.
    fn polling(source: &Path) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let config = notify::Config::default().with_poll_interval(POLL_INTERVAL).with_compare_contents(true);
        let mut watcher = PollWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if res.is_ok_and(|e| is_change(&e.kind)) {
                    let _ = tx.send(());
                }
            },
            config,
        )?;
        watcher.watch(source, RecursiveMode::NonRecursive).with_context(|| format!("polling {}", source.display()))?;
        Ok(Self { rx, _watcher: Box::new(watcher), mode: "polling" })
    }

    /// Wait up to `timeout` for a change, then let the rest of that save's events arrive.
    fn wait(&self, timeout: Duration) -> Wait {
        match self.rx.recv_timeout(timeout) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => return Wait::Idle,
            Err(RecvTimeoutError::Disconnected) => return Wait::Stopped,
        }
        // One save is several events (write, rename, attributes); rebuild once, after the last.
        while self.rx.recv_timeout(SETTLE).is_ok() {}
        Wait::Changed
    }
}

/// Events that can change the file's contents. Reading the file (to rebuild the page)
/// produces access events on Linux, so those must not count.
fn is_change(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Any | EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_))
}

/// Rebuild and reload on every save, until the tab is closed.
pub fn run(page: &Page, tab: &Tab, poll: bool, log: &Log) {
    let changes = match Changes::new(&page.source, poll, log) {
        Ok(changes) => changes,
        Err(err) => {
            log.say(format_args!("can't watch {}: {err:#}", page.source.display()));
            return;
        }
    };
    log.say(format_args!("reloading on every save of {} ({})", page.source.display(), changes.mode));
    // The page was just written from the file as it is now.
    let mut rendered = page.read().ok().map(|(_, hash)| hash);
    // One missing answer can be a browser hiccup, so the tab must be missing twice in a row.
    let mut missing = 0;
    loop {
        match changes.wait(TAB_CHECK) {
            Wait::Stopped => {
                log.say("the file watcher stopped; not reloading any more");
                return;
            }
            Wait::Idle => {}
            Wait::Changed => match page.read() {
                Ok((_, hash)) if rendered == Some(hash) => continue,
                Ok((markdown, hash)) => {
                    if let Err(err) = page.write(&markdown) {
                        log.say(format_args!("{err:#}"));
                        continue;
                    }
                    rendered = Some(hash);
                    if browser::reload(tab) {
                        missing = 0;
                        continue;
                    }
                    log.say("couldn't reload the browser tab; will retry on the next save");
                }
                // Editors that delete and rewrite the file leave it missing for a moment.
                Err(err) if is_not_found(&err) => continue,
                Err(err) => {
                    log.say(format_args!("{err:#}"));
                    continue;
                }
            },
        }
        missing = if browser::is_open(tab) { 0 } else { missing + 1 };
        if missing >= 2 {
            log.say("browser tab closed; stopping");
            return;
        }
    }
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// Where a background watcher writes its messages.
pub fn log_path(output: &Path) -> PathBuf {
    output.with_extension("log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Instant;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tmdview-watch-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    /// Whether a change arrives within `within`, ignoring events from before the call.
    fn changed(changes: &Changes, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            if let Wait::Changed = changes.wait(left) {
                return true;
            }
        }
    }

    fn drain(changes: &Changes) {
        while changed(changes, Duration::from_millis(300)) {}
    }

    fn check_saves(poll: bool) {
        let dir = temp_dir(if poll { "poll" } else { "events" });
        let file = dir.join("notes.md");
        fs::write(&file, "one").unwrap();
        let log = Log::new(dir.join("log"));
        let changes = Changes::new(&file, poll, &log).unwrap();
        drain(&changes);

        fs::write(&file, "two").unwrap();
        assert!(changed(&changes, Duration::from_secs(3)), "a plain write");
        drain(&changes);

        // How Vim and most editors save: write a temp file, rename it over the original.
        fs::write(dir.join(".notes.md.swp"), "three").unwrap();
        fs::rename(dir.join(".notes.md.swp"), &file).unwrap();
        assert!(changed(&changes, Duration::from_secs(3)), "a rename-over save");
        drain(&changes);

        fs::write(&file, "four").unwrap();
        assert!(changed(&changes, Duration::from_secs(3)), "a write after the rename");
        drain(&changes);

        let _ = fs::read_to_string(&file).unwrap();
        fs::write(dir.join("other.md"), "x").unwrap();
        assert!(!changed(&changes, Duration::from_millis(800)), "reading the file or changing another one");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_events_catch_every_kind_of_save() {
        check_saves(false);
    }

    #[test]
    fn polling_catches_every_kind_of_save() {
        check_saves(true);
    }
}
