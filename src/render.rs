//! Markdown -> standalone HTML document.

use std::collections::HashSet;
use std::path::Path;

use pulldown_cmark::{html, CodeBlockKind, CowStr, Event, MetadataBlockKind, Options, Parser, Tag, TagEnd};
use syntect::html::{ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::plugins::Loaded;

const TEMPLATE: &str = include_str!("template.html");
const CLASS_STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };

pub struct Renderer {
    syntaxes: SyntaxSet,
    /// The terminal's colors and font for `--theme terminal`, and the config file's
    /// code colors.
    theme_css: String,
    /// The config file's plugin tables, as a JSON object.
    plugin_config: String,
    plugins: Vec<Loaded>,
}

impl Renderer {
    /// `theme_css` overrides the page's colors and fonts. `plugin_config` reaches the
    /// page's scripts as `tmdview.config`.
    pub fn new(plugins: Vec<Loaded>, theme_css: String, plugin_config: String) -> Self {
        Self { syntaxes: SyntaxSet::load_defaults_newlines(), theme_css, plugin_config, plugins }
    }

    /// Render `markdown` into a full HTML page. `base_dir` is used so relative
    /// links and images resolve against the markdown file's directory.
    pub fn render(&self, markdown: &str, title: &str, base_dir: &Path, theme: &str) -> String {
        let Body { html: body, mut used, front_matter } = self.render_body(markdown);
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
            .replace("{{theme_css}}", &self.theme_css)
            .replace("{{body}}", &body);
        // `<` can't appear raw, so no value can close the script tag.
        page.push_str(&format!("<script>tmdview.config = {};</script>\n", self.plugin_config.replace('<', "\\u003c")));
        if let Some(front_matter) = &front_matter {
            page.push_str(&front_matter.script());
            let keys = front_matter.keys();
            for (i, plugin) in self.plugins.iter().enumerate() {
                if plugin.wants_front_matter(&keys) && !used.contains(&i) {
                    used.push(i);
                }
            }
        }
        for &i in &used {
            page.push_str(&self.plugins[i].assets());
        }
        page.push_str(tail);
        page
    }

    fn render_body(&self, markdown: &str) -> Body {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_SMART_PUNCTUATION
            | Options::ENABLE_HEADING_ATTRIBUTES
            | Options::ENABLE_GFM
            | Options::ENABLE_MATH
            // Front matter (`---` YAML or `+++` TOML at the top) is data for other tools, not
            // page content. The HTML writer leaves these blocks out.
            | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
            | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS;

        let mut events = Vec::new();
        let mut code: Option<(String, String, String)> = None; // (lang, rest of the info string, source)
        let mut front_matter: Option<FrontMatter> = None;
        let mut in_front_matter = false;
        let mut used: Vec<usize> = Vec::new();
        let mut heading: Option<(Tag, Vec<Event>)> = None; // (start tag, inner events)
        let mut cell: Option<Vec<Event>> = None; // a table cell's inner events
        // Explicit `{#id}`s are reserved up front so generated slugs never collide with them.
        let mut used_ids: HashSet<String> = Parser::new_ext(markdown, options)
            .filter_map(|e| match e {
                Event::Start(Tag::Heading { id: Some(id), .. }) => Some(id.to_string()),
                _ => None,
            })
            .collect();

        for (event, range) in Parser::new_ext(markdown, options).into_offset_iter() {
            // Code blocks: buffer text, then emit highlighted HTML.
            if let Some((lang, info, src)) = code.as_mut() {
                match event {
                    Event::Text(t) => src.push_str(&t),
                    Event::End(TagEnd::CodeBlock) => {
                        let html = self.code_block(lang, info, src, &mut used);
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
                    let (lang, info) = match &kind {
                        CodeBlockKind::Fenced(info) => {
                            let info = info.trim();
                            let (lang, rest) = info.split_once(char::is_whitespace).unwrap_or((info, ""));
                            (lang.to_string(), rest.trim().to_string())
                        }
                        CodeBlockKind::Indented => (String::new(), String::new()),
                    };
                    code = Some((lang, info, String::new()));
                }
                // Front matter isn't page content; plugins get it as data.
                Event::Start(Tag::MetadataBlock(kind)) => {
                    front_matter = Some(FrontMatter { kind, source: String::new() });
                    in_front_matter = true;
                }
                Event::Text(t) if in_front_matter => {
                    front_matter.as_mut().expect("set at the block's start").source.push_str(&t);
                }
                Event::End(TagEnd::MetadataBlock(_)) => in_front_matter = false,
                Event::Start(Tag::Heading { level, id, classes, attrs }) => {
                    let tag = match heading_attrs(&markdown[range]) {
                        Some(parsed) => parsed.into_tag(level, id),
                        None => Tag::Heading { level, id, classes, attrs },
                    };
                    heading = Some((tag, Vec::new()));
                }
                Event::Start(Tag::TableCell) => {
                    events.push(event);
                    cell = Some(Vec::new());
                }
                Event::End(TagEnd::TableCell) => {
                    events.extend(no_wrap(cell.take().unwrap_or_default()));
                    events.push(event);
                }
                Event::InlineMath(m) => cell.as_mut().unwrap_or(&mut events).push(Event::Html(
                    format!("<span class=\"math\">{}</span>", escape(&m)).into(),
                )),
                Event::DisplayMath(m) => cell.as_mut().unwrap_or(&mut events).push(Event::Html(
                    format!("<div class=\"math math-display\">{}</div>", escape(&m)).into(),
                )),
                other => cell.as_mut().unwrap_or(&mut events).push(other),
            }
        }

        let mut out = String::with_capacity(markdown.len() * 2);
        html::push_html(&mut out, events.into_iter());
        Body { html: out, used, front_matter }
    }

    /// A fenced code block. In order: a plugin that claims its language, syntect,
    /// a fallback plugin, then plain escaped text. Records which plugin it used.
    fn code_block(&self, lang: &str, info: &str, src: &str, used: &mut Vec<usize>) -> String {
        let claimed = self.plugins.iter().position(|p| p.claims(lang));
        if claimed.is_none()
            && let Some(html) = self.highlight(lang, src)
        {
            return html;
        }
        let fallback = || (!lang.is_empty()).then(|| self.plugins.iter().position(|p| p.fallback)).flatten();
        let label = label(lang);
        match claimed.or_else(fallback) {
            Some(i) => {
                if !used.contains(&i) {
                    used.push(i);
                }
                let name = escape(&self.plugins[i].name);
                // The rest of the info string, like `{fill=1}`, for plugins that take options.
                let info = if info.is_empty() { String::new() } else { format!(" data-info=\"{}\"", escape(info)) };
                format!("<pre class=\"tmdview-plugin\" data-plugin=\"{name}\"{label}{info}><code>{}</code></pre>\n", escape(src))
            }
            None => format!("<pre{label}><code>{}</code></pre>\n", escape(src)),
        }
    }

    /// Syntax-colored HTML, or `None` if syntect doesn't know the language.
    fn highlight(&self, lang: &str, src: &str) -> Option<String> {
        let syntax = (!lang.is_empty()).then(|| self.syntaxes.find_syntax_by_token(lang)).flatten()?;
        let mut generator = ClassedHTMLGenerator::new_with_class_style(syntax, &self.syntaxes, CLASS_STYLE);
        for line in LinesWithEndings::from(src) {
            generator.parse_html_for_line_which_includes_newline(line).ok()?;
        }
        Some(format!("<pre class=\"hl-code\"{}><code>{}</code></pre>\n", label(lang), generator.finalize()))
    }
}

/// A heading's trailing `{#id .class key=value key="a value"}`.
struct HeadingAttrs {
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<(String, Option<String>)>,
}

impl HeadingAttrs {
    /// `key=value` pairs become `data-key` attributes, so they can't clash with the
    /// heading's `id` or give it a `title` tooltip. Plugins read them from `dataset`.
    fn into_tag(self, level: pulldown_cmark::HeadingLevel, id: Option<CowStr>) -> Tag {
        Tag::Heading {
            level,
            id: self.id.map(CowStr::from).or(id),
            classes: self.classes.into_iter().map(CowStr::from).collect(),
            attrs: self.attrs.into_iter().map(|(k, v)| (format!("data-{k}").into(), v.map(CowStr::from))).collect(),
        }
    }
}

/// Read the attributes at the end of a heading's first source line. pulldown-cmark
/// finds them but doesn't understand quoted values, so `{title="Mapa de planta"}`
/// would come apart at the spaces.
fn heading_attrs(source: &str) -> Option<HeadingAttrs> {
    let line = source.lines().next()?.trim_end().trim_end_matches('#').trim_end();
    let inner = line.strip_suffix('}')?;
    let inner = &inner[inner.rfind('{')? + 1..];
    let mut parsed = HeadingAttrs { id: None, classes: Vec::new(), attrs: Vec::new() };
    let mut rest = inner.trim_start();
    while !rest.is_empty() {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if let Some(id) = rest.strip_prefix('#') {
            parsed.id = Some(id[..end - 1].to_string());
            rest = &rest[end..];
        } else if let Some(class) = rest.strip_prefix('.') {
            parsed.classes.push(class[..end - 1].to_string());
            rest = &rest[end..];
        } else {
            let key_end = rest.find(|c: char| c == '=' || c.is_whitespace()).unwrap_or(rest.len());
            let key = &rest[..key_end];
            if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
                return None;
            }
            rest = &rest[key_end..];
            let value = match rest.strip_prefix('=') {
                None => None,
                Some(after) => {
                    let (value, len) = match after.chars().next() {
                        Some(q @ ('"' | '\'')) => {
                            let close = after[1..].find(q)?;
                            (&after[1..close + 1], close + 2)
                        }
                        _ => {
                            let len = after.find(char::is_whitespace).unwrap_or(after.len());
                            (&after[..len], len)
                        }
                    };
                    rest = &after[len..];
                    Some(value.to_string())
                }
            };
            parsed.attrs.push((key.to_string(), value));
        }
        rest = rest.trim_start();
    }
    Some(parsed)
}

/// A rendered page body, the indexes of the plugins that claimed a block in it, and its front matter.
struct Body {
    html: String,
    used: Vec<usize>,
    front_matter: Option<FrontMatter>,
}

/// A `---` YAML or `+++` TOML block at the top of the file.
struct FrontMatter {
    kind: MetadataBlockKind,
    source: String,
}

impl FrontMatter {
    /// The block as JSON in the page, for plugins: `{"format": "yaml", "source": "…"}`.
    fn script(&self) -> String {
        let format = match self.kind {
            MetadataBlockKind::YamlStyle => "yaml",
            MetadataBlockKind::PlusesStyle => "toml",
        };
        let json = serde_json::json!({ "format": format, "source": self.source }).to_string();
        // `<` can't appear raw, so nothing in the block can close the script tag.
        let json = json.replace('<', "\\u003c");
        format!("<script type=\"application/json\" id=\"tmdview-front-matter\">{json}</script>\n")
    }

    /// The top-level keys, which decide the plugins a page gets. YAML keys are read from
    /// the lines that start a `key:` at the left margin, which is all this needs, with no
    /// YAML parser. TOML is parsed, since the toml crate is already here.
    fn keys(&self) -> Vec<String> {
        match self.kind {
            MetadataBlockKind::YamlStyle => self
                .source
                .lines()
                .filter(|line| !line.starts_with([' ', '\t', '#', '-']))
                .filter_map(|line| line.split_once(':'))
                .map(|(key, _)| key.trim().trim_matches(['"', '\'']).to_string())
                .filter(|key| !key.is_empty())
                .collect(),
            MetadataBlockKind::PlusesStyle => {
                toml::from_str::<toml::Table>(&self.source).map(|t| t.keys().cloned().collect()).unwrap_or_default()
            }
        }
    }
}

fn label(lang: &str) -> String {
    if lang.is_empty() { String::new() } else { format!(" data-lang=\"{}\"", escape(lang)) }
}

/// A table cell's contents, kept on one line when they have no spaces, like an ID, a
/// date or a version. Otherwise a narrow column breaks `D-01` after the hyphen.
fn no_wrap(inner: Vec<Event>) -> Vec<Event> {
    let text = plain_text(&inner);
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return inner;
    }
    let mut out = Vec::with_capacity(inner.len() + 2);
    out.push(Event::InlineHtml("<span class=\"nowrap\">".into()));
    out.extend(inner);
    out.push(Event::InlineHtml("</span>".into()));
    out
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

/// `file://` URL for an absolute path.
pub fn file_url(path: &Path) -> String {
    format!("file://{}", percent_encode(path.to_string_lossy().as_bytes()))
}

/// Percent-encode everything but unreserved chars and `/`.
pub fn percent_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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
        Renderer::new(Vec::new(), String::new(), "{}".into()).render_body(md).html
    }

    #[test]
    fn mermaid_blocks_stay_code_without_the_plugin() {
        let html = body("```mermaid\ngraph LR; a-->b\n```\n");
        assert!(html.contains(r#"<pre data-lang="mermaid"><code>"#), "{html}");
    }

    #[test]
    fn plugin_claims_blocks_and_adds_its_assets_once() {
        let renderer = Renderer::new(vec![plugin("mermaid", &["mermaid"], false)], String::new(), "{}".into());
        let md = "```mermaid\ngraph LR; a-->b\n```\n\n```mermaid\ngraph TD; c-->d\n```\n";
        let page = renderer.render(md, "t", Path::new("/"), "auto");
        let block = r#"<pre class="tmdview-plugin" data-plugin="mermaid" data-lang="mermaid"><code>graph LR; a--&gt;b"#;
        assert!(page.contains(block), "{page}");
        assert_eq!(page.matches("/* mermaid script */").count(), 1);
        assert_eq!(page.matches("/* mermaid style */").count(), 1);
        assert!(!page.contains("{{plugins}}"));
    }

    #[test]
    fn plugins_skip_pages_without_their_blocks() {
        let renderer = Renderer::new(vec![plugin("mermaid", &["mermaid"], false)], String::new(), "{}".into());
        let page = renderer.render("# Hi\n", "t", Path::new("/"), "auto");
        assert!(!page.contains("mermaid script"));
    }

    #[test]
    fn claimed_languages_beat_syntect_and_fallback_only_takes_the_rest() {
        let renderer = Renderer::new(vec![plugin("hl", &["python"], true)], String::new(), "{}".into());
        let html = renderer.render_body("```python\nx\n```\n\n```rust\nfn f() {}\n```\n\n```zig\nx\n```\n\n```\nx\n```\n").html;
        assert!(html.contains(r#"data-plugin="hl" data-lang="python""#), "{html}");
        assert!(html.contains(r#"class="hl-code" data-lang="rust""#), "{html}");
        assert!(html.contains(r#"data-plugin="hl" data-lang="zig""#), "{html}");
        assert!(html.contains("<pre><code>x\n</code></pre>"), "a block with no language stays plain: {html}");
    }

    fn plugin(name: &str, languages: &[&str], fallback: bool) -> Loaded {
        Loaded {
            name: name.into(),
            languages: languages.iter().map(|l| l.to_string()).collect(),
            fallback,
            front_matter_keys: Vec::new(),
            scripts: vec![format!("/* {name} script */")],
            styles: vec![format!("/* {name} style */")],
        }
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
    fn heading_attributes_take_quoted_values_and_become_data_attributes() {
        let html = body("# Mapa de planta {id=\"A1\" crumb='Anexo' title=\"Mapa de planta\" ref=mapa}\n\n## Two {#two .wide open}\n");
        assert!(
            html.contains(r#"<h1 id="mapa-de-planta" data-id="A1" data-crumb="Anexo" data-title="Mapa de planta" data-ref="mapa">Mapa de planta</h1>"#),
            "{html}"
        );
        assert!(html.contains(r#"<h2 id="two" class="wide" data-open="">Two</h2>"#), "{html}");
        assert!(body("# Plain {not attrs\n").contains("<h1 id=\"plain-not-attrs\">Plain {not attrs</h1>"));
    }

    #[test]
    fn front_matter_is_left_out() {
        let html = body("---\ntitle: Spec\nmeta:\n  a: 1\n---\n\n# Hi\n");
        assert_eq!(html, "<h1 id=\"hi\">Hi</h1>\n");
        let html = body("+++\ntitle = \"Spec\"\n+++\n\nText\n");
        assert_eq!(html, "<p>Text</p>\n");
    }

    #[test]
    fn front_matter_reaches_the_page_and_the_plugins_that_want_it() {
        let mut mts = plugin("mts", &[], false);
        mts.front_matter_keys = vec!["eyebrow".into()];
        let renderer = Renderer::new(vec![mts, plugin("other", &[], false)], String::new(), "{}".into());
        let md = "---\ntitle: A </script> B\neyebrow: Spec\nmeta:\n  eyebrow: nested\n---\n\n# Hi\n";
        let page = renderer.render(md, "t", Path::new("/"), "auto");
        let script = r#"<script type="application/json" id="tmdview-front-matter">{"format":"yaml","source":"title: A \u003c/script> B\neyebrow: Spec"#;
        assert!(page.contains(script), "{page}");
        assert_eq!(page.matches("/* mts script */").count(), 1);
        assert!(!page.contains("other script"));
        assert!(!renderer.render("# Hi\n", "t", Path::new("/"), "auto").contains("mts script"), "no front matter, no plugin");
    }

    #[test]
    fn front_matter_keys_are_the_top_level_ones() {
        let yaml = FrontMatter { kind: MetadataBlockKind::YamlStyle, source: "title: x\nmeta:\n  Linear: y\n# note: z\n- item\n\"quoted\": 1\nstyle: |\n  .a { b: c }\n".into() };
        assert_eq!(yaml.keys(), ["title", "meta", "quoted", "style"]);
        let toml = FrontMatter { kind: MetadataBlockKind::PlusesStyle, source: "title = \"x\"\n[meta]\na = 1\n".into() };
        assert_eq!(toml.keys(), ["meta", "title"]);
    }

    #[test]
    fn plugin_blocks_keep_the_rest_of_the_info_string() {
        let renderer = Renderer::new(vec![plugin("mts", &["kpi"], false)], String::new(), "{}".into());
        let html = renderer.render_body("```kpi {fill=1,3}\n| a | 1 |\n```\n").html;
        assert!(html.contains(r#"data-plugin="mts" data-lang="kpi" data-info="{fill=1,3}">"#), "{html}");
    }

    #[test]
    fn table_cells_without_spaces_stay_on_one_line() {
        let html = body("| ID | What |\n|---|---|\n| D-01 | Two words |\n| `x-y` | $a$ |\n");
        assert!(html.contains(r#"<td><span class="nowrap">D-01</span></td>"#), "{html}");
        assert!(html.contains("<td>Two words</td>"), "{html}");
        assert!(html.contains(r#"<td><span class="nowrap"><code>x-y</code></span></td>"#), "{html}");
        assert!(html.contains(r#"<span class="math">a</span>"#), "math in a cell still renders: {html}");
    }

    #[test]
    fn gfm_extensions() {
        let html = body("| a |\n|---|\n| b |\n\n- [x] done\n\n~~gone~~\n");
        assert!(html.contains("<table>"));
        assert!(html.contains("checkbox"));
        assert!(html.contains("<del>gone</del>"));
    }

    #[test]
    fn plugin_config_reaches_the_page_and_cant_close_its_script() {
        let renderer = Renderer::new(Vec::new(), String::new(), r#"{"mermaid":{"x":"</script>"}}"#.into());
        let page = renderer.render("# Hi\n", "t", Path::new("/"), "auto");
        assert!(page.contains(r#"<script>tmdview.config = {"mermaid":{"x":"\u003c/script>"}};</script>"#), "{page}");
    }
}
