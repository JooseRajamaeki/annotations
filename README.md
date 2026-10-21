# annotator

Joose's minimal annotation tool for code. Open a file in the terminal, select some lines, write a note about them, and annotator saves it as a small Markdown file. That's all it does.

There is no server, database, account or config file. Each annotation is a plain `<UUID>.md` file in an `annotations/` directory, which you can read, grep, commit or pass on to someone else (or to another tool).

## Install

annotator is written in Rust and needs Rust 1.89 or newer.

```sh
cargo install light-annotator
```

This installs the `annotator` command. To build from source instead:

```sh
git clone https://github.com/JooseRajamaeki/annotations.git
cd annotations
cargo install --path .
```

## Usage

```sh
annotator                 # browse files, starting in the current directory
annotator src/main.rs     # open a file
```

In a file, select lines and press `c` to comment on them. Press `Esc` to save the comment, or `Ctrl+Q` to discard it.

The second line of the header always shows the main keys for what you are doing, and `?` lists them all.

| Key | Action |
|---|---|
| `j`/`k`, `↑`/`↓` | move |
| `v` | select (paint) lines |
| `c` / `Enter` | comment on the selected line(s) |
| `f` | comment on the whole file |
| `b` | browse files; `c` there comments on a file or directory |
| `q` | quit |

The mouse works too. Scroll with the wheel, click to move to a line, and drag to select lines. In the file browser, click to select an entry and click it again to open it. Because annotator captures the mouse, selecting text in the terminal needs your terminal's modifier key (usually Shift, or Option in iTerm2).

### Without the interface

```sh
annotator src/main.rs --lines 12-18 --note "This could be a loop"
annotator --path src/ --note "Split this module"
```

Annotations go to `./annotations/` by default; use `--output-dir DIR` to
change this.

## What an annotation looks like

````markdown
# Annotation

- scope: `src/main.rs#L12-18`
- git_commit: `071ab73ab157b8e8c7c8c0ad9b487a8a92b5ccda`
- content_sha256: `72a19e6b24f4453ada78e7d1ed430d461ce35aa1668c75d529cebd39f8e8053b`
- created_at: 2026-10-21T11:25:06.865264+00:00

## Context

```rs
<the selected lines>
```

## Comment

This could be a loop
````

- **scope**: what the note is about: lines (`src/main.rs#L12-18`), a file
  (`src/main.rs`) or a directory (`src/`).
- **git_commit**: the commit checked out when the note was written, or
  `unavailable` outside a git repository.
- **content_sha256**: a hash of the whole file (or, for a directory, of all its
  files). It shows whether the code has changed since the note was written,
  including uncommitted changes.
- **Context**: a copy of the selected lines, so the note still makes sense
  after the code changes. Only line annotations have it.

## License

MIT, see [LICENSE](LICENSE).
