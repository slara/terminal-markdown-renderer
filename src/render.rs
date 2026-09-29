//! Markdown -> standalone HTML document.

use std::collections::HashSet;
use std::path::Path;

use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use syntect::highlighting::ThemeSet;
use syntect::html::{css_for_theme_with_class_style, ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::plugins::{Loaded, Plugin};

const TEMPLATE: &str = include_str!("template.html");
const CLASS_STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };

pub struct Renderer {
    syntaxes: SyntaxSet,
    highlight_css: String,
    plugins: Vec<Loaded>,
}

impl Renderer {
    pub fn new(plugins: Vec<Loaded>) -> anyhow::Result<Self> {
        let themes = ThemeSet::load_defaults();
        let light = css_for_theme_with_class_style(&themes.themes["InspiredGitHub"], CLASS_STYLE)?;
        let dark = css_for_theme_with_class_style(&themes.themes["base16-ocean.dark"], CLASS_STYLE)?;
        let highlight_css = format!(
            "{light}\n@media (prefers-color-scheme: dark) {{\n{}\n}}\n{}\n",
            scope_css(&dark, ":root:not([data-theme=\"light\"])"),
            scope_css(&dark, ":root[data-theme=\"dark\"]"),
        );
        Ok(Self { syntaxes: SyntaxSet::load_defaults_newlines(), highlight_css, plugins })
    }

    /// Render `markdown` into a full HTML page. `base_dir` is used so relative
    /// links and images resolve against the markdown file's directory.
    pub fn render(&self, markdown: &str, title: &str, base_dir: &Path, theme: &str) -> String {
        let (body, used) = self.render_body(markdown);
        let base = format!("{}/", file_url(base_dir));
        let theme_attr = match theme {
            "light" | "dark" => format!(" data-theme=\"{theme}\""),
            _ => String::new(),
        };
        // Split first, so neither the body nor a plugin's script is searched for placeholders.
        let (head, tail) = TEMPLATE.split_once("{{plugins}}").expect("template has {{plugins}}");
        let mut page = head
            .replace("{{theme_attr}}", &theme_attr)
            .replace("{{title}}", &escape(title))
            .replace("{{base}}", &escape(&base))
            .replace("{{highlight_css}}", &self.highlight_css)
            .replace("{{body}}", &body);
        for loaded in self.plugins.iter().filter(|l| used.contains(&l.plugin)) {
            page.push_str(&loaded.plugin.scripts(&loaded.library));
        }
        page.push_str(tail);
        page
    }

    /// The page body, plus the plugins that claimed at least one block in it.
    fn render_body(&self, markdown: &str) -> (String, Vec<Plugin>) {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_SMART_PUNCTUATION
            | Options::ENABLE_HEADING_ATTRIBUTES
            | Options::ENABLE_GFM
            | Options::ENABLE_MATH;

        let mut events = Vec::new();
        let mut code: Option<(String, String)> = None; // (lang, source)
        let mut used: Vec<Plugin> = Vec::new();
        let mut heading: Option<(Tag, Vec<Event>)> = None; // (start tag, inner events)
        // Explicit `{#id}`s are reserved up front so generated slugs never collide with them.
        let mut used_ids: HashSet<String> = Parser::new_ext(markdown, options)
            .filter_map(|e| match e {
                Event::Start(Tag::Heading { id: Some(id), .. }) => Some(id.to_string()),
                _ => None,
            })
            .collect();

        for event in Parser::new_ext(markdown, options) {
            // Code blocks: buffer text, then emit highlighted HTML.
            if let Some((lang, src)) = code.as_mut() {
                match event {
                    Event::Text(t) => src.push_str(&t),
                    Event::End(TagEnd::CodeBlock) => {
                        let html = match self.plugins.iter().map(|l| l.plugin).find(|p| p.claims(lang)) {
                            Some(plugin) => {
                                if !used.contains(&plugin) {
                                    used.push(plugin);
                                }
                                plugin.block(src)
                            }
                            None => self.highlight(lang, src),
                        };
                        code = None;
                        events.push(Event::Html(html.into()));
                    }
                    _ => {}
                }
                continue;
            }
            // Headings: buffer inner events to derive an anchor id.
            if let Some((_, inner)) = heading.as_mut() {
                if let Event::End(TagEnd::Heading(_)) = event {
                    let (Tag::Heading { level, id, classes, attrs }, inner) = heading.take().unwrap() else {
                        unreachable!()
                    };
                    let id = id.unwrap_or_else(|| unique_slug(&plain_text(&inner), &mut used_ids).into());
                    events.push(Event::Start(Tag::Heading { level, id: Some(id), classes, attrs }));
                    events.extend(inner);
                    events.push(event);
                } else {
                    inner.push(event);
                }
                continue;
            }
            match event {
                Event::Start(Tag::CodeBlock(kind)) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    code = Some((lang, String::new()));
                }
                Event::Start(tag @ Tag::Heading { .. }) => heading = Some((tag, Vec::new())),
                Event::InlineMath(m) => events.push(Event::Html(
                    format!("<span class=\"math\">{}</span>", escape(&m)).into(),
                )),
                Event::DisplayMath(m) => events.push(Event::Html(
                    format!("<div class=\"math math-display\">{}</div>", escape(&m)).into(),
                )),
                other => events.push(other),
            }
        }

        let mut out = String::with_capacity(markdown.len() * 2);
        html::push_html(&mut out, events.into_iter());
        (out, used)
    }

    fn highlight(&self, lang: &str, src: &str) -> String {
        let syntax = (!lang.is_empty())
            .then(|| self.syntaxes.find_syntax_by_token(lang))
            .flatten();
        let label = if lang.is_empty() { String::new() } else { format!(" data-lang=\"{}\"", escape(lang)) };
        let Some(syntax) = syntax else {
            return format!("<pre{label}><code>{}</code></pre>\n", escape(src));
        };
        let mut generator = ClassedHTMLGenerator::new_with_class_style(syntax, &self.syntaxes, CLASS_STYLE);
        for line in LinesWithEndings::from(src) {
            if generator.parse_html_for_line_which_includes_newline(line).is_err() {
                return format!("<pre{label}><code>{}</code></pre>\n", escape(src));
            }
        }
        format!("<pre class=\"hl-code\"{label}><code>{}</code></pre>\n", generator.finalize())
    }
}

/// Prefix every selector in syntect's generated CSS with `scope`.
fn scope_css(css: &str, scope: &str) -> String {
    css.lines()
        .map(|line| match line.split_once('{') {
            Some((selectors, rest)) if line.trim_start().starts_with('.') => {
                let scoped: Vec<String> =
                    selectors.split(',').map(|s| format!("{scope} {}", s.trim())).collect();
                format!("{} {{{rest}", scoped.join(", "))
            }
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn plain_text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Text(t) | Event::Code(t) => Some(t.as_ref()),
            _ => None,
        })
        .collect()
}

fn unique_slug(text: &str, used: &mut HashSet<String>) -> String {
    let mut slug = String::new();
    for c in text.trim().to_lowercase().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            slug.push(c);
        } else if c.is_whitespace() {
            slug.push('-');
        }
    }
    let mut candidate = slug.clone();
    let mut n = 0;
    while used.contains(&candidate) {
        n += 1;
        candidate = format!("{slug}-{n}");
    }
    used.insert(candidate.clone());
    candidate
}

/// `file://` URL for an absolute path, percent-encoding everything but unreserved chars and `/`.
pub fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for &b in path.to_string_lossy().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(md: &str) -> String {
        Renderer::new(Vec::new()).unwrap().render_body(md).0
    }

    #[test]
    fn mermaid_blocks_stay_code_without_the_plugin() {
        let html = body("```mermaid\ngraph LR; a-->b\n```\n");
        assert!(html.contains(r#"<pre data-lang="mermaid"><code>"#), "{html}");
    }

    #[test]
    fn mermaid_plugin_claims_blocks_and_adds_its_script_once() {
        let renderer = Renderer::new(vec![fake_mermaid()]).unwrap();
        let md = "```mermaid\ngraph LR; a-->b\n```\n\n```mermaid\ngraph TD; c-->d\n```\n";
        let page = renderer.render(md, "t", Path::new("/"), "auto");
        assert!(page.contains("<pre class=\"mermaid\">graph LR; a--&gt;b\n</pre>"), "{page}");
        assert_eq!(page.matches("/* mermaid library */").count(), 1);
        assert_eq!(page.matches("window.tmdviewReady = mermaid.run").count(), 1);
        assert!(!page.contains("{{plugins}}"));
    }

    #[test]
    fn mermaid_plugin_skips_pages_without_diagrams() {
        let renderer = Renderer::new(vec![fake_mermaid()]).unwrap();
        let page = renderer.render("# Hi\n", "t", Path::new("/"), "auto");
        assert!(!page.contains("mermaid library"));
    }

    fn fake_mermaid() -> Loaded {
        Loaded { plugin: Plugin::Mermaid, library: "/* mermaid library */".into() }
    }

    #[test]
    fn headings_get_unique_anchor_ids() {
        let html = body("# Hello World\n\n## Hello World\n");
        assert!(html.contains(r#"<h1 id="hello-world">Hello World</h1>"#), "{html}");
        assert!(html.contains(r#"<h2 id="hello-world-1">"#), "{html}");
    }

    #[test]
    fn generated_ids_avoid_explicit_ones() {
        let html = body("## Intro\n\n# Title {#intro}\n");
        assert!(html.contains(r#"<h2 id="intro-1">"#), "{html}");
        assert!(html.contains(r#"<h1 id="intro">"#), "{html}");
    }

    #[test]
    fn file_urls_are_percent_encoded() {
        assert_eq!(file_url(Path::new("/a b/C#/é?.md")), "file:///a%20b/C%23/%C3%A9%3F.md");
    }

    #[test]
    fn code_blocks_are_highlighted() {
        let html = body("```rust\nfn main() {}\n```\n");
        assert!(html.contains(r#"class="hl-code" data-lang="rust""#), "{html}");
        assert!(html.contains("hl-"), "{html}");
    }

    #[test]
    fn unknown_language_is_escaped_plain() {
        let html = body("```nope\n<b>\n```\n");
        assert!(html.contains("<pre data-lang=\"nope\"><code>&lt;b&gt;\n</code></pre>"), "{html}");
    }

    #[test]
    fn gfm_extensions() {
        let html = body("| a |\n|---|\n| b |\n\n- [x] done\n\n~~gone~~\n");
        assert!(html.contains("<table>"));
        assert!(html.contains("checkbox"));
        assert!(html.contains("<del>gone</del>"));
    }
}
