# unpeel-markdown

A beautiful terminal markdown editor built with [Ratatui](https://ratatui.rs).
Live block styling, a headings picker, slash commands on empty lines
(`/` for headings, lists, to-dos, quotes, code, dividers), `[] ` to start a
to-do, full mouse support (click, drag-select, double/triple-click), and a
searchable Markdown Explorer when you open a folder.

Edits auto-save after a short typing pause. The footer shows the current
state; click it to toggle auto-save, or choose **Toggle auto-save** from the
`\\` command palette. The preference persists across launches, and `Ctrl+S`
always saves manually.

```
unpeel-markdown notes/hello.md    # edit one file
unpeel-markdown notes/            # vault mode: scoped Markdown Explorer
unpeel-markdown                   # remembered notes folder: open or create
```

The first bare launch asks you to choose a notes folder. Later bare launches
always return to that folder's searchable note list, which includes a
**New note** action (`Ctrl+N`). The picker is App Kit's shared scoped
`Explorer`: it keeps folders navigable, admits only `.md` files, and cannot
leave the chosen notes root. Its shared borderless input supports a native
cursor, word movement, Shift selection, paste, mouse drag selection, and
double-click word selection. Typing while the file list is active immediately
focuses and writes into the filter; click the filter or press Up from the first
row to place the cursor there, then press Down or Tab to return to the list.
Rows keep the two-cell inset, full-width adaptive gray selection, double-click
activation, path dragging, and proportional scrollbar. No current directory
or bundled demo is opened implicitly. The
choice uses the same persisted start-state shape as Unpeel Design under
`~/.config/unpeel-apps/unpeel.app.markdown/start.json` (or `$XDG_CONFIG_HOME`
/ `$UNPEEL_APP_CONFIG_HOME`). A command-line path always bypasses the launcher.

Standalone first: it is a complete editor in any terminal with no Unpeel
present. When [Unpeel](https://unpeel.com) is installed, Unpeel recognizes
the `unpeel-markdown` CLI directly from `PATH`: the session row takes the
App's name and live project/workspace accent, and the sidebar shows which note
you're editing. No App registry write is required.

The Unpeel integration remains the documented plain-file plus local HTTP
contract, implemented once by `unpeel-app-kit`'s `AppReporter`. Reusable UI
also comes from App Kit: live dark/light/accent colors, gray selection,
keyboard mode, scrollbars, native drop destinations, and `MarkdownTextArea`.

## Install

```sh
curl -fsSL https://unpeel.com/install/markdown/install.sh | sh
```

The checksum-verified installer places the CLI on `PATH`; Unpeel discovers it
without running the App or mutating `~/.unpeel`.

To build and install from source, keep App Kit beside the App repository:

```sh
mkdir -p ~/Dev && cd ~/Dev
git clone https://github.com/unpeel-com/unpeel-app-kit.git
git clone https://github.com/unpeel-com/unpeel-app-markdown.git
cargo install --locked --path unpeel-app-markdown
```

## Development

```sh
cargo run -- demo.md   # the editor on the bundled demo document
cargo run -- .         # vault mode on the current directory
cargo test
```

The editor follows the terminal's light or dark palette at startup. Set
`UNPEEL_TUI_THEME=light` or `UNPEEL_TUI_THEME=dark` to override the shared
App Kit theme detection.

Drag a file or folder anywhere over the editor body to preview its exact
insertion caret. Hovering near the top or bottom edge auto-scrolls, and the
drop inserts at that caret. This uses App Kit's reusable semantic drop-target
surface; outside Unpeel, ordinary terminal paste behavior remains available.

Ported from the `markdown-editor` experiment in `unpeel-experiments`.
