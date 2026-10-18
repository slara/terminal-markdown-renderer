#!/usr/bin/env bash
# herdr plugin action: open a Markdown file in tmdview, beside the focused pane.
# The file is the clicked file:// link, else the selected text, else README.md
# in the focused pane's folder.
set -euo pipefail

# ponytail: sed, not a JSON parser, so a value with an escaped quote comes out cut short.
field() {
  sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p" <<<"${HERDR_PLUGIN_CONTEXT_JSON:-}"
}

cwd=$(field focused_pane_cwd)
cwd=${cwd:-$(field workspace_cwd)}
url=${HERDR_PLUGIN_CLICKED_URL:-}
if [[ -n $url ]]; then
  # file://host/path: ls --hyperlink and eza put the hostname in.
  path=${url#file://}
  path=/${path#*/}
  path=${path%%#*}
  path=$(printf '%b' "${path//%/\\x}")
else
  path=$(field selected_text)
  # A selection often takes the line break or spaces around it.
  path=${path//\\n/}
  path=${path#"${path%%[![:space:]]*}"}
  path=${path%"${path##*[![:space:]]}"}
  [[ $path == "~/"* ]] && path=$HOME/${path#"~/"}
  path=${path:-README.md}
fi
[[ $path == /* ]] || path="${cwd:-$PWD}/$path"

bin="${HERDR_PLUGIN_ROOT:-}/target/release/tmdview"
[[ -x $bin ]] || bin=tmdview
exec "$bin" "$path" --split right
