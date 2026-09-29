# Writing a plugin

A tmdview plugin is a git repository with a manifest and some JavaScript or CSS.
tmdview inlines those files into every page that has a code block the plugin takes over.
Use it for a diagram type, a notation or a code language tmdview doesn't handle.

[tmdview-highlight](https://github.com/slara/tmdview-highlight) is a complete example.

## Layout

```
tmdview-plugin.toml   the manifest
init.js               finds the plugin's blocks and draws them
vendor/lib.min.js     the library that does the drawing, if any
styles.css            optional
```

Commit the library files in the repository.
tmdview runs no build step, and it doesn't download anything for a git plugin besides the clone.

## The manifest

```toml
name = "d2"                                  # lowercase letters, digits and dashes
version = "0.1.0"                            # shown in `plugins list`
description = "Draws ```d2 blocks"           # optional, shown before install

languages = ["d2"]                           # code block languages to take over
fallback = false                             # also take blocks nothing else handles

styles = ["styles.css"]                      # inlined in <style> tags, in order
scripts = ["vendor/d2.min.js", "init.js"]    # inlined in <script> tags, in order
```

- Set `languages`, `fallback = true`, or both.
- List at least one file in `scripts` or `styles`. Paths are relative to the repository and must stay inside it.
- A plugin can't use a built-in plugin's name (`mermaid`).
- Unknown keys are an error, so a typo doesn't go unnoticed.

## Which blocks a plugin gets

For each fenced code block, tmdview uses the first of these that applies:

1. A plugin that lists the block's language in `languages`. This also works for languages tmdview can color itself.
2. tmdview's own syntax colors.
3. The plugin with `fallback = true`, for any block that has a language.
4. Plain text.

Two installed plugins can't claim the same language, and only one plugin can be the fallback.
`install` and `update` refuse a plugin that would break either rule.

## What the page contains

Each block the plugin takes over becomes:

```html
<pre class="tmdview-plugin" data-plugin="d2" data-lang="d2"><code>…the block's text, HTML-escaped…</code></pre>
```

Read the source with `code.textContent`.
The plugin's styles and scripts go at the end of `<body>`, after every block, so `init.js` can run straight away:

```js
(() => {
  for (const pre of document.querySelectorAll('pre[data-plugin="d2"]')) {
    const svg = D2.render(pre.textContent);   // whatever your library does
    pre.replaceChildren(svg);
    pre.removeAttribute("data-lang");          // hides the language label in the corner
  }
})();
```

If drawing finishes later, pass its promise to `tmdview.ready(promise)`.
When tmdview reloads the page in `--watch` mode, it waits for those promises before it restores your scroll position, since drawing changes the page height.

## Light and dark themes

The page is dark when `<html data-theme="dark">` is set, or when the system is dark and `data-theme="light"` isn't set.
Scope dark styles like this:

```css
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) pre.tmdview-plugin .thing { color: #e6edf3; }
}
:root[data-theme="dark"] pre.tmdview-plugin .thing { color: #e6edf3; }
```

In a script:

```js
const forced = document.documentElement.dataset.theme;
const dark = forced ? forced === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
```

The page's colors are CSS variables on `:root`, such as `--fg`, `--bg`, `--subtle-bg`, `--border` and `--link`.
Use them to match the page.

## Trying it

`install` takes a local path, so you can try a plugin before you push it:

```sh
tmdview plugins install ../tmdview-d2
tmdview notes.md -s right -w
```

The installed copy is a clone.
After you commit a change, run `tmdview plugins update d2` to pick it up.

## Keep in mind

- The page is opened as a `file://` URL, and it's meant to work offline. Don't load anything from the network.
- An inlined `</script` would end the script early, so tmdview rewrites it as `<\/script`. Inside a JavaScript string or regex that means the same thing. It does the same for `</style` in CSS.
- A script that contains both `<!--` and `<script` is added as a base64 `data:` URL instead of inline, because the HTML parser could otherwise misread where it ends. It runs the same either way.
- Your code runs in every page that uses the plugin, and users are warned about that before they install it.
