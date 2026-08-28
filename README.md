# unpeel-markdown

A beautiful terminal markdown editor built with [Ratatui](https://ratatui.rs).
Live block styling, a headings picker, slash commands on empty lines
(`/` for headings, lists, to-dos, quotes, code, dividers), `[] ` to start a
to-do, full mouse support (click, drag-select, double/triple-click), and a
searchable Markdown Explorer when you open a folder.

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
present. When [Unpeel](https://unpeel.com) is installed it also registers
itself as an Unpeel App (one manifest under
`~/.unpeel/apps/unpeel.app.markdown/`): the session row takes the app's name
and blue tint — even when you just type `unpeel-markdown` into any Unpeel
terminal — and the sidebar shows which note you're editing.

The Unpeel integration remains plain files plus one tiny local HTTP contract
in `src/unpeel.rs` and `src/install.rs`. Reusable UI comes from
`../unpeel-app-kit`: shared dark/light colors, gray selection, keyboard mode,
scrollbars, and the Ratatui `MarkdownTextArea` component.

## Install

```sh
curl -fsSL https://unpeel.com/install/markdown/install.sh | sh
```

The checksum-verified installer registers the versioned App manifest
immediately under `~/.unpeel/apps/unpeel.app.markdown/`.

To build and install from source, keep App Kit beside the App repository:

```sh
mkdir -p ~/Dev && cd ~/Dev
git clone https://github.com/unpeel-com/unpeel-app-kit.git
git clone https://github.com/unpeel-com/unpeel-app-markdown.git
cargo install --locked --path unpeel-app-markdown
unpeel-markdown --register
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

Running any command once self-installs the App manifest into
`~/.unpeel/apps/unpeel.app.markdown/` with that binary's absolute path as
the launch command during development, or the stable `unpeel-markdown` name
when it is installed on `PATH`. Its command/process aliases let Unpeel detect
a hand-typed launch and apply the app identity, tint, and live
`editing <file>` status just like Unpeel Design.

Ported from the `markdown-editor` experiment in `unpeel-experiments`.
