# annotator

Joose's minimal annotation tool for code. Open a file in the terminal, select some lines, write a note about them, and annotator saves it as a small Markdown file. That's all it does.

There is no server, database, account or config file. Each annotation is a plain `<UUID>.md` file in an `annotations/` directory, which you can read, grep, commit or pass on to someone else (or to another tool).

## Install

annotator is written in Rust and needs Rust 1.89 or newer.

```sh
cargo install annotations
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

The first time you save, annotator asks your name and writes it into every annotation from then on, until you quit. You can leave it blank; annotator then asks once more to make sure.

The second line of the header always shows the main keys for what you are doing, and `?` lists them all.

| Key | Action |
|---|---|
| `j`/`k`, `↑`/`↓` | move (`g`/`G` top/bottom) |
| `v` | select (paint) lines |
| `c` / `Enter` | comment on the selected line(s) |
| `f` | comment on the whole file |
| `m` | show a Markdown file rendered or as source |
| `o` | open a link on the line (Markdown) |
| `b` | browse files; `c` there comments on a file or directory |
| `q` | quit |

Markdown files open rendered: headings, emphasis, links, lists, quotes and
tables are drawn without their markup. Each source line stays on its own line
with its own number, so you select and comment on the file as written. Press `m`
to see the source.

Hover over a link to see where it goes. Click a link on the cursor line, or press
`o`, to open it. Web and mail links open in your browser or mail app. Links to
files and directories open in annotator, and `#heading` links jump to the
heading. Other links, such as `file:` ones, are not opened, so a link cannot
start a program.

The mouse works too. Scroll with the wheel, click to move to a line, and drag to select lines. In the file browser, click to select an entry and click it again to open it. While writing a comment, click in the comment box to place the cursor. Because annotator captures the mouse, selecting text in the terminal needs your terminal's modifier key (usually Shift, or Option in iTerm2).

### Without the interface

```sh
annotator src/main.rs --lines 12-18 --note "This could be a loop"
annotator --path src/ --note "Split this module"
annotator --path src/ --note "Split this module" --user "Joose"
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
- user: Joose

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
- **user**: who wrote the note, or `unavailable` if they left their name blank.
- **Context**: a copy of the selected lines, so the note still makes sense
  after the code changes. Only line annotations have it.

## Feedback

Found a bug or have an idea for annotator? Open an issue at
https://github.com/JooseRajamaeki/annotations/issues

Design decisions are recorded in
[architectural-decision-record.md](architectural-decision-record.md).

## License

MIT, see [LICENSE](LICENSE).

---

Edited by Claude Opus 5.5 a6ce3c44-4635-42c0-b6c3-48e952313d1c
Edited by Mistral Vibe d2783d7a-6011-9d53-ac2c-5c3a8804ace6
Edited by Mistral Vibe mistral-large-4 1fd3b04c-5d71-4583-34b1-6fe588bbfe5d
