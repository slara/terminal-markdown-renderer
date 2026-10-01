//! The endpoint that opens a linked Markdown file. A page can't open one itself: the
//! browser would show its raw text. So a click on a link to a `.md` file asks the
//! background watcher, at `<url>/render?path=<file>`, to render it, and gets back the
//! `file://` URL of its page to go to. Browsers block a redirect from http to file://,
//! so the page navigates itself.
//!
//! It listens on localhost only, and the URL carries a random token, so another web
//! page can't use it.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{Log, Page, browser, render};

/// A fresh endpoint URL, `http://127.0.0.1:<port>/<token>`, for the watcher to serve.
// ponytail: the port is free now, but the watcher binds it a moment later; pass the
// socket itself if that race ever shows up.
pub fn new_url() -> Result<String> {
    let port = TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).context("finding a free port")?.port();
    let token = RandomState::new().build_hasher().finish();
    Ok(format!("http://127.0.0.1:{port}/{token:016x}"))
}

/// Answer render requests for as long as the process runs.
pub fn run(url: &str, page: &Page, log: &Log) {
    let (addr, token) = url.trim_start_matches("http://").split_once('/').expect("made by new_url");
    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        Err(err) => return log.say(format_args!("can't serve links to Markdown files on {addr}: {err}")),
    };
    for stream in listener.incoming().flatten() {
        if let Err(err) = answer(stream, token, page) {
            log.say(format_args!("{err:#}"));
        }
    }
}

fn answer(mut stream: TcpStream, token: &str, page: &Page) -> Result<()> {
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let (status, body) = match requested_file(&line, token) {
        Some(source) => match page.write_linked(&source) {
            Ok(output) => ("200 OK", render::file_url(&output)),
            Err(err) => ("500 Internal Server Error", format!("{err:#}")),
        },
        None => ("404 Not Found", String::new()),
    };
    // File pages send `Origin: null`, which `*` lets read the answer.
    write!(
        stream,
        "HTTP/1.1 {status}\r\nAccess-Control-Allow-Origin: *\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(())
}

/// The Markdown file a `GET /<token>/render?path=<file> HTTP/1.1` line asks for, if
/// the token matches and the file exists.
fn requested_file(line: &str, token: &str) -> Option<PathBuf> {
    let target = line.strip_prefix("GET ")?.split(' ').next()?;
    let path = target.strip_prefix('/')?.strip_prefix(token)?.strip_prefix("/render?path=")?;
    let source = Path::new(&browser::percent_decode(path)).canonicalize().ok()?;
    let ext = source.extension()?.to_str()?.to_ascii_lowercase();
    (source.is_file() && (ext == "md" || ext == "markdown")).then_some(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_existing_markdown_files_with_the_token() {
        let dir = std::env::temp_dir().join(format!("tmdview-serve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Guía de uso.md"), "# Hi\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let ask = |token: &str, file: &str| {
            let path = render::percent_encode(dir.join(file).to_string_lossy().as_bytes());
            requested_file(&format!("GET /{token}/render?path={path} HTTP/1.1\r\n"), "abc")
        };
        let found = ask("abc", "Guía de uso.md");
        let others = [ask("abd", "Guía de uso.md"), ask("abc", "notes.txt"), ask("abc", "missing.md")];
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(found.unwrap().ends_with("Guía de uso.md"));
        assert!(others.iter().all(Option::is_none), "{others:?}");
    }
}
