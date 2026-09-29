//! Thin wrapper over the `terminal-browser` CLI.

use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;

const BIN: &str = "terminal-browser";

/// A tab in a running terminal-browser, addressable by `action`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub browser: String,
    pub id: u64,
}

/// `<browser key>/<tab id>`, the form the background watcher gets on its command line.
impl fmt::Display for Tab {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/{}", self.browser, self.id)
    }
}

impl FromStr for Tab {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (browser, id) = s.rsplit_once('/').ok_or_else(|| format!("expected <browser>/<tab>, not {s:?}"))?;
        let id = id.parse().map_err(|_| format!("the tab id in {s:?} isn't a number"))?;
        Ok(Tab { browser: browser.to_string(), id })
    }
}

/// A tab plus the local file it shows, if it shows one.
struct Listed {
    tab: Tab,
    path: Option<String>,
}

/// Open `page` in a new split pane beside the current one. Returns once the pane is up.
pub fn open_split(page: &Path, direction: &str, size: Option<f32>) -> Result<()> {
    let mut cmd = Command::new(BIN);
    cmd.arg("open").arg(page).args(["--split", direction]);
    if let Some(size) = size {
        cmd.args(["--size", &size.to_string()]);
    }
    let out = cmd.stdout(Stdio::null()).output().with_context(|| format!("running `{BIN}` (is it installed?)"))?;
    if !out.status.success() {
        bail!("`{BIN} open` failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Open `page` in the current pane. The browser usually takes over the terminal
/// until the user quits it, but may instead exit at once after adding a tab to a
/// neighbouring browser.
pub fn open_here(page: &Path) -> Result<Child> {
    Command::new(BIN)
        .arg("open")
        .arg(page)
        .spawn()
        .with_context(|| format!("running `{BIN}` (is it installed?)"))
}

/// Every tab currently open, so a later `find_new_tab` can tell new ones apart.
pub fn snapshot() -> Vec<Tab> {
    list().unwrap_or_default().into_iter().map(|l| l.tab).collect()
}

/// Find the tab showing `page` that wasn't in `before`, retrying while the browser
/// starts up. Falls back to any tab showing `page`.
pub fn find_new_tab(page: &Path, before: &[Tab]) -> Option<Tab> {
    let page = page.to_string_lossy();
    let mut any = None;
    for _ in 0..40 {
        let showing: Vec<Tab> = list()
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.path.as_deref() == Some(&*page))
            .map(|l| l.tab)
            .collect();
        if let Some(tab) = showing.iter().find(|t| !before.contains(t)) {
            return Some(tab.clone());
        }
        any = showing.into_iter().next().or(any);
        thread::sleep(Duration::from_millis(150));
    }
    any
}

/// Whether `tab` is still open. Errs on the side of "yes" if listing fails.
pub fn is_open(tab: &Tab) -> bool {
    list().map_or(true, |tabs| tabs.iter().any(|l| &l.tab == tab))
}

fn list() -> Result<Vec<Listed>> {
    let out = Command::new(BIN).args(["ls", "--json"]).stderr(Stdio::null()).output()?;
    let json: Value = serde_json::from_slice(&out.stdout)?;
    let mut tabs = Vec::new();
    for browser in json["browsers"].as_array().into_iter().flatten() {
        let Some(key) = browser["key"].as_str() else { continue };
        for tab in browser["tabs"].as_array().into_iter().flatten() {
            let Some(id) = tab["id"].as_u64() else { continue };
            let path = tab["url"].as_str().and_then(|u| u.strip_prefix("file://")).map(percent_decode);
            tabs.push(Listed { tab: Tab { browser: key.to_string(), id }, path });
        }
    }
    Ok(tabs)
}

/// Decode `%XX` escapes, so urls compare equal however the browser chose to encode them.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reload a tab. Returns false if the tab is gone (e.g. the user closed it).
pub fn reload(tab: &Tab) -> bool {
    let ok = action(tab, &["--", "reload"]);
    // Clear the "agent is driving this tab" indicator right away.
    action(tab, &["done"]);
    ok
}

fn action(tab: &Tab, args: &[&str]) -> bool {
    Command::new(BIN)
        .args(["action", "--browser", &tab.browser, "--tab", &tab.id.to_string()])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_roundtrips_encoded_paths() {
        assert_eq!(percent_decode("/a%20b/C%23/%C3%A9.html"), "/a b/C#/é.html");
        assert_eq!(percent_decode("/plain/100%"), "/plain/100%");
    }
}
