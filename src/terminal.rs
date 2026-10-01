//! The terminal's own look, for `--theme terminal`: its background and text colors
//! and, where the terminal tells us, its font. Links, highlights and code keep the page's GitHub
//! colors, Dark or Light to match the background.
//!
//! Colors come from asking the terminal itself (OSC 10 and 11), which any modern
//! terminal answers. Whatever it doesn't answer comes from Ghostty's config, when
//! running in Ghostty. The font only comes from Ghostty's config, since terminals have
//! no way to report it.

use std::process::{Child, Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// `self` moved `amount` (0 to 1) of the way towards `other`.
    fn mix(self, other: Rgb, amount: f32) -> Rgb {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }

    /// Relative luminance, 0 (black) to 1 (white).
    fn luminance(self) -> f32 {
        let lin = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(self.0) + 0.7152 * lin(self.1) + 0.0722 * lin(self.2)
    }

    /// `#rgb`, `#rrggbb`, or X11's `rgb:r/g/b` with 1 to 4 hex digits per channel.
    fn parse(s: &str) -> Option<Rgb> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            let v = |i: usize, n: usize| u8::from_str_radix(hex.get(i..i + n)?, 16).ok();
            return match hex.len() {
                6 => Some(Rgb(v(0, 2)?, v(2, 2)?, v(4, 2)?)),
                3 => Some(Rgb(v(0, 1)? * 17, v(1, 1)? * 17, v(2, 1)? * 17)),
                _ => None,
            };
        }
        let mut channels = s.strip_prefix("rgb:")?.split('/').map(|c| {
            let max = 16u32.checked_pow(u32::try_from(c.len()).ok().filter(|n| (1..=4).contains(n))?)? - 1;
            Some((u32::from_str_radix(c, 16).ok()? * 255 / max) as u8)
        });
        let rgb = Rgb(channels.next()??, channels.next()??, channels.next()??);
        channels.next().is_none().then_some(rgb)
    }
}

/// What the page takes from the terminal. It travels to the background watcher as JSON
/// on its command line, since the watcher has no terminal to ask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub bg: Rgb,
    pub fg: Rgb,
    pub font: Option<String>,
}

/// Colors as reported, any of which may be missing.
#[derive(Default)]
struct Found {
    bg: Option<Rgb>,
    fg: Option<Rgb>,
    font: Option<String>,
}

impl Found {
    fn fill_from(&mut self, other: Found) {
        self.bg = self.bg.or(other.bg);
        self.fg = self.fg.or(other.fg);
        self.font = self.font.take().or(other.font);
    }
}

/// The terminal's style, or `None` if neither the terminal nor its config gave a
/// background and foreground.
pub fn detect() -> Option<Style> {
    // Started first so it runs while the terminal answers. It's always needed in Ghostty,
    // since only the config has the font.
    let ghostty = spawn_ghostty_config();
    let mut found = query_tty().unwrap_or_default();
    if let Some(config) = ghostty.and_then(|child| child.wait_with_output().ok()).filter(|out| out.status.success()) {
        found.fill_from(parse_ghostty_config(&String::from_utf8_lossy(&config.stdout)));
    }
    Some(Style { bg: found.bg?, fg: found.fg?, font: found.font })
}

impl Style {
    fn is_dark(&self) -> bool {
        self.bg.luminance() < self.fg.luminance()
    }

    /// The page theme that matches: `dark` or `light`.
    pub fn scheme(&self) -> &'static str {
        if self.is_dark() { "dark" } else { "light" }
    }

    /// CSS that sets the page's background, text and font from the terminal.
    pub fn css(&self) -> String {
        let (bg, fg) = (self.bg, self.fg);
        let font = match &self.font {
            Some(name) => format!("\"{}\", ", css_string(name)),
            None => String::new(),
        };
        format!(
            r#":root[data-theme] {{
  --bg: {bg};
  --fg: {fg};
  --muted: {muted};
  --border: {border};
  --subtle-bg: {subtle};
  --quote: {muted};
  --font: {font}var(--mono);
}}
{CSS}"#,
            bg = bg.hex(),
            fg = fg.hex(),
            muted = fg.mix(bg, 0.4).hex(),
            border = fg.mix(bg, 0.75).hex(),
            subtle = bg.mix(fg, 0.06).hex(),
        )
    }
}

/// The part of the terminal theme that doesn't depend on the terminal. Links,
/// highlights and code keep the page's GitHub colors, Dark or Light from `data-theme`.
const CSS: &str = r#"/* Text keeps GitHub's type; code takes the terminal's font. */
code, pre, kbd { font-family: var(--font); }
"#;

/// Keep a font name from ending the CSS string it goes in or the `<style>` tag, and from
/// looking like one of the template's `{{placeholders}}`.
fn css_string(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '"' | '\\' | '<' | '>' | '{' | '}' | '\n' | '\r')).collect()
}

/// Ask the terminal for its colors. The query ends with a device attributes request,
/// which every terminal answers, so a terminal that ignores the color queries costs
/// one round trip, not a timeout.
#[cfg(unix)]
fn query_tty() -> Option<Found> {
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    let mut tty = OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    let fd = tty.as_raw_fd();
    // SAFETY: termios is plain data, and `fd` is an open terminal for as long as `tty` lives.
    let saved = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return None;
        }
        t
    };
    let mut raw = saved;
    raw.c_lflag &= !(libc::ICANON | libc::ECHO);
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 1; // reads return after 0.1 s without input
    // SAFETY: as above.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return None;
    }

    // Foreground, background, then device attributes.
    let query = "\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c";
    let mut reply = Vec::new();
    if tty.write_all(query.as_bytes()).and_then(|()| tty.flush()).is_ok() {
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut buf = [0u8; 1024];
        while Instant::now() < deadline && !ends_with_device_attributes(&reply) {
            match tty.read(&mut buf) {
                Ok(n) => reply.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    }
    // SAFETY: as above.
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
    Some(parse_replies(&String::from_utf8_lossy(&reply)))
}

#[cfg(not(unix))]
fn query_tty() -> Option<Found> {
    None
}

/// Whether `reply` has the answer to `ESC [ c`, which is `ESC [ ? … c`.
fn ends_with_device_attributes(reply: &[u8]) -> bool {
    let Some(start) = reply.windows(3).rposition(|w| w == b"\x1b[?") else { return false };
    reply[start + 3..].contains(&b'c')
}

/// Pull the colors out of the terminal's OSC replies, which end in `ESC \` or BEL.
fn parse_replies(reply: &str) -> Found {
    let mut found = Found::default();
    for part in reply.split("\x1b]").skip(1) {
        let part = part.split(['\x07', '\x1b']).next().unwrap_or("");
        let mut fields = part.split(';');
        match (fields.next(), fields.next(), fields.next()) {
            (Some("10"), Some(color), None) => found.fg = Rgb::parse(color),
            (Some("11"), Some(color), None) => found.bg = Rgb::parse(color),
            _ => {}
        }
    }
    found
}

/// Start printing Ghostty's effective config, when running in Ghostty.
fn spawn_ghostty_config() -> Option<Child> {
    let bin = match std::env::var_os("GHOSTTY_BIN_DIR") {
        Some(dir) => std::path::Path::new(&dir).join("ghostty"),
        None if std::env::var("TERM_PROGRAM").as_deref() == Ok("ghostty") => "ghostty".into(),
        None => return None,
    };
    Command::new(bin).arg("+show-config").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()
}

fn parse_ghostty_config(config: &str) -> Found {
    let mut found = Found::default();
    for line in config.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        match key.trim() {
            "background" => found.bg = Rgb::parse(value),
            "foreground" => found.fg = Rgb::parse(value),
            // The first one is the main font; later ones are fallbacks.
            "font-family" if found.font.is_none() && !value.is_empty() => {
                found.font = Some(value.trim_matches('"').to_string());
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_in_every_form() {
        assert_eq!(Rgb::parse("#222222"), Some(Rgb(0x22, 0x22, 0x22)));
        assert_eq!(Rgb::parse("#fa0"), Some(Rgb(0xff, 0xaa, 0x00)));
        assert_eq!(Rgb::parse("rgb:2222/c5c5/0000"), Some(Rgb(0x22, 0xc5, 0x00)));
        assert_eq!(Rgb::parse("rgb:f/8/0"), Some(Rgb(0xff, 0x88, 0x00)));
        assert_eq!(Rgb::parse("rgb:22/c5"), None);
        assert_eq!(Rgb::parse("rgb:22222/0/0"), None);
        assert_eq!(Rgb::parse("red"), None);
    }

    #[test]
    fn terminal_replies_give_colors() {
        let reply = "\x1b]10;rgb:c5c5/c8c8/c6c6\x1b\\\x1b]11;rgb:2222/2222/2222\x07\x1b]4;4;rgb:8585/bebe/fdfd\x1b\\\x1b[?62;22c";
        let found = parse_replies(reply);
        assert_eq!(found.fg, Some(Rgb(0xc5, 0xc8, 0xc6)));
        assert_eq!(found.bg, Some(Rgb(0x22, 0x22, 0x22)));
        assert!(ends_with_device_attributes(reply.as_bytes()));
        assert!(!ends_with_device_attributes(b"\x1b]11;rgb:0/0/0\x1b\\"));
    }

    #[test]
    fn ghostty_config_gives_colors_and_the_first_font() {
        let config = "font-family = MesloLGS Nerd Font Mono\nfont-family = Apple Color Emoji\nbackground = #222222\nforeground = #c5c8c6\npalette = 2=#87c38a\npalette = 200=#000000\n";
        let found = parse_ghostty_config(config);
        assert_eq!(found.font.as_deref(), Some("MesloLGS Nerd Font Mono"));
        assert_eq!(found.bg, Some(Rgb(0x22, 0x22, 0x22)));
    }

    #[test]
    fn css_uses_the_terminal_colors_and_a_safe_font_name() {
        let dark = Style { bg: Rgb(0x22, 0x22, 0x22), fg: Rgb(0xc5, 0xc8, 0xc6), font: Some("Evil\"</style>".into()) };
        assert!(dark.is_dark());
        let css = dark.css();
        assert!(css.contains("--bg: #222222;"), "{css}");
        assert!(css.contains("--font: \"Evil/style\", var(--mono);"), "{css}");
        let light = Style { bg: Rgb(0xff, 0xff, 0xff), fg: Rgb(0x1f, 0x23, 0x28), font: None };
        assert_eq!(light.scheme(), "light", "so the page takes GitHub Light's colors");
    }

    #[test]
    fn style_survives_the_trip_to_the_watcher() {
        let style = Style { bg: Rgb(1, 2, 3), fg: Rgb(4, 5, 6), font: None };
        let json = serde_json::to_string(&style).unwrap();
        assert_eq!(serde_json::from_str::<Style>(&json).unwrap(), style);
    }
}
