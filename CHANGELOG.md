# Changelog

## Unreleased

### Added

- A copy button on each code block. It shows on hover and says "Copied" once the code is on the clipboard.

## 0.5.0 — 2026-10-01

### Changed

- Built-in plugins download with your own `curl`, which ships with macOS and most Linux systems. `tmdview plugins install mermaid` now needs it.
- Two fewer dependencies (`ureq` and `dirs`), and 22 fewer crates in the build. Plugins stay in the same data folder, so installed ones keep working.
- A plugin script with both `<!--` and `<script` goes in as a percent-encoded `data:` URL instead of a base64 one. It runs the same.

## 0.4.0 — 2026-09-30

### Added

- Plugins can list `front_matter_keys` to run on pages whose front matter has one of them. The front matter reaches the page as JSON.
- Plugin blocks carry the rest of the fence's info string in `data-info`.
- Heading attributes accept quoted values (`{title="Mapa de planta"}`) and become `data-*` attributes.

### Changed

- The terminal theme takes the background, text color and font from the terminal, and GitHub Dark or Light colors for links, highlights and code. It no longer asks the terminal for its ANSI palette.

Older versions reject plugin manifests that use `front_matter_keys`.

## 0.3.0 — 2026-09-30

### Added

- `--theme terminal` matches the terminal's colors, and in Ghostty its font.
- `--poll` watches by polling, for filesystems that don't report changes.

### Changed

- Every opened page reloads on save. A background copy of tmdview watches the file with OS events (inotify on Linux, FSEvents on macOS), catches editors that save by renaming, and stops when the tab closes. In split mode the shell stays free.
- `--no-watch` turns reloading off. `-w`/`--watch` are removed.
- The side margins grow with the pane.

### Fixed

- Front matter (`---` YAML or `+++` TOML at the top) no longer shows up as a rule, a paragraph and a heading.
- Table cells with no spaces, like `D-01`, stay on one line in narrow columns.

## 0.2.0 — 2026-09-29

### Added

- Plugins: `tmdview plugins list`, `install`, `update` and `remove`. Mermaid is built in, as a pinned, checksummed download. Installed plugins are inlined only into pages that use them. `--no-plugins` skips them for one run.
- Plugins from git repositories with a `tmdview-plugin.toml` manifest (`owner/repo`, a URL or a local path, with an optional `--ref`). They are cloned with your own `git`, so private repositories work. tmdview shows what a plugin takes over and asks before installing or updating.
- `docs/PLUGINS.md`, the guide for plugin authors.

### Fixed

- `plugins update` checks the new commit in a throwaway clone, so the installed plugin doesn't change unless you agree, even if you press Ctrl+C at the prompt.
- A plugin script containing both `<!--` and `<script` can no longer swallow the rest of the page.

## 0.1.0 — 2026-09-29

First release: renders a Markdown file to HTML and shows it in terminal-browser, in the current pane or a split.
