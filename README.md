# unpeel-markdown

A beautiful terminal markdown editor built with [Ratatui](https://ratatui.rs).
Live block styling, a headings picker, slash commands on empty lines
(`/` for headings, lists, to-dos, quotes, code, dividers), `[] ` to start a
to-do, full mouse support (click, drag-select, double/triple-click), and a
searchable note picker when you open a folder.

```
unpeel-markdown notes/hello.md    # edit one file
unpeel-markdown notes/            # vault mode: fuzzy note picker
unpeel-markdown                   # current directory as a vault
```

Standalone first: it is a complete editor in any terminal with no Unpeel
present. When [Unpeel](https://unpeel.com) is installed it also registers
itself as an Unpeel App (one manifest under
`~/.unpeel/apps/unpeel.app.markdown/`): the session row takes the app's name
and blue tint — even when you just type `unpeel-markdown` into any Unpeel
terminal — and the sidebar shows which note you're editing.

There is no SDK: the whole integration is `src/unpeel.rs` and
`src/install.rs`, plain files and one tiny local HTTP contract. The editor
itself is ordinary Ratatui (`tui-textarea` for the buffer).

Install:

```sh
curl -fsSL https://unpeel.com/install/markdown/install.sh | sh
```

Ported from the `markdown-editor` experiment in `unpeel-experiments`.
