# Architectural decision record

This file persists the architectural choices of annotator. Open work and bug
reports live in GitHub issues: https://github.com/JooseRajamaeki/annotations/issues

It replaces the design-decisions section of PLAN.md, removed in
https://github.com/JooseRajamaeki/annotations/pull/13.

## Decisions

### annotator is a terminal UI built with ratatui and crossterm

Painting a range of lines maps naturally to a vim-like visual selection in
the terminal. The UI is modal and lives in one TUI session: browsing, viewing
and commenting all happen there. Each mode shows its main keys on the header
line, and `?` lists every key.

### Markdown files render in place

`.md` and `.markdown` files open rendered; `m` switches to the source. Each
source line is drawn on its own rows with its own line number, so painting,
the mouse and saved scopes all refer to the source lines. The cost is that
hard-wrapped paragraphs are not reflowed and setext headings are not joined.

### Links open only through the system opener or inside annotator

http(s) and mailto links go to the system opener. Relative paths and
`#anchor` links open in annotator. Every other scheme is refused, because a
link must not be able to start a program.

### An annotation has one scope

A line range (`src/foo.rs#L3-7`), a whole file (`src/foo.rs`) or a whole
directory (`src/`). A directory gets one annotation, not one per file. Paths
inside the working directory are stored relative to it.

### Storage is one Markdown file per annotation

Annotations are written as `<UUID>.md` into an `annotations/` directory
(configurable with `--output-dir`). There is no server, database, account or
config file.

### Metadata is collected at save time

- the git commit of the repository containing the annotated file, if any
- SHA-256 of the whole file; for a directory, SHA-256 of a manifest with one line per file
- the scope reference
- a UTC timestamp
- the user name (`--user` in batch mode, asked once per TUI session)

### Output format

````markdown
# Annotation

- scope: `path/to/file.rs#L12-18`   (or `path/to/file.rs`, `path/to/dir/`)
- git_commit: <hash or "unavailable">
- content_sha256: <hash>
- created_at: <RFC3339 UTC>
- user: <name or "unavailable">

## Context                (only for line ranges)

```rs                     (language tag = file extension)
<the annotated lines>
```

## Comment

<note text>
````

## Dependencies

- ratatui + crossterm — the TUI
- uuid — annotation file names
- sha2 — content hashing
- chrono — timestamps

---

Created by Mistral Vibe mistral-large-4 1fd3b04c-5d71-4583-34b1-6fe588bbfe5d
