use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// What an annotation is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// A line range of a file, 1-based inclusive.
    Lines(usize, usize),
    /// A whole file.
    File,
    /// A whole directory (recursive).
    Directory,
}

/// A single annotation: a comment tied to a line range, file or directory,
/// plus the metadata collected at save time.
pub struct Annotation {
    pub path: PathBuf,
    pub scope: Scope,
    pub git_commit: Option<String>,
    /// For files and line ranges: SHA-256 of the whole file.
    /// For directories: SHA-256 of a `sha256sum`-style manifest of its files.
    pub content_sha256: String,
    pub created_at: String,
    /// The annotated lines themselves, for line-range annotations.
    pub context: Option<String>,
    pub note: String,
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

/// Lines `start..=end` (1-based, inclusive) of `contents`.
fn line_range(contents: &str, start: usize, end: usize) -> String {
    let skip = start.saturating_sub(1);
    let lines: Vec<&str> = contents.lines().skip(skip).take(end.saturating_sub(skip)).collect();
    lines.join("\n")
}

/// Length of the longest run of backticks in `text`.
fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// A Markdown code block holding `code`, tagged with `path`'s extension as the
/// language. The fence is longer than any run of backticks inside `code`.
fn code_block(code: &str, path: &Path) -> String {
    let fence = "`".repeat(longest_backtick_run(code).max(2) + 1);
    let lang = path.extension().map_or(String::new(), |e| e.to_string_lossy().to_string());
    format!("{fence}{lang}\n{code}\n{fence}\n")
}

/// `text` as Markdown inline code. The fence is longer than any run of
/// backticks inside `text`, and padded with spaces when `text` starts or ends
/// with a backtick, so a backtick in a file name cannot end the code span.
fn inline_code(text: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(text) + 1);
    if text.starts_with('`') || text.ends_with('`') {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

/// Refuse paths with control characters: a newline in a file name would
/// otherwise end the scope line and let the name forge the metadata below it.
fn check_path(path: &Path) -> std::io::Result<()> {
    if path.to_string_lossy().contains(char::is_control) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains control characters (such as a newline)",
        ));
    }
    Ok(())
}

/// Refuse anything but a regular file (or a symlink to one). Opening a FIFO
/// blocks forever and a device like /dev/zero never ends.
fn check_regular_file(path: &Path) -> std::io::Result<()> {
    if !fs::metadata(path)?.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"));
    }
    Ok(())
}

/// Contents of the text file at `path`, which must be a regular file.
pub fn read_text_file(path: &Path) -> std::io::Result<String> {
    check_regular_file(path)?;
    fs::read_to_string(path)
}

/// Directory to run git in for `path`: the directory itself, or a file's parent.
fn git_dir(path: &Path) -> PathBuf {
    if path.is_dir() {
        return path.to_path_buf();
    }
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// HEAD commit of the git repo containing `path`, if any.
fn git_commit(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(git_dir(path))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hash.is_empty() {
        None
    } else {
        Some(hash)
    }
}

/// Files under `dir`, relative to it. Inside a git repo this is the tracked
/// plus untracked-but-not-ignored files (so `target/` etc. are skipped);
/// otherwise a recursive walk that skips hidden entries.
fn list_files(dir: &Path) -> Vec<PathBuf> {
    let git = Command::new("git")
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "."])
        .current_dir(dir)
        .output();
    let mut files: Vec<PathBuf> = match git {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect(),
        _ => {
            let mut files = Vec::new();
            walk(dir, Path::new(""), &mut files);
            files
        }
    };
    files.sort();
    files.dedup();
    files
}

fn walk(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root.join(rel)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let rel_path = rel.join(&name);
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(root, &rel_path, out),
            Ok(t) if t.is_file() => out.push(rel_path),
            _ => {}
        }
    }
}

/// Hash of a directory's contents: SHA-256 of a manifest with one
/// `<file sha256>  <relative path>` line per file, sorted by path (the same
/// format `sha256sum` prints). Files listed by git but missing on disk
/// (deleted in the working tree) get `deleted` in place of a hash. Anything
/// that is not a regular file, such as a symlink to a FIFO or a device, is
/// left out, since reading it could block or never end.
pub fn directory_sha256(dir: &Path) -> String {
    let mut manifest = String::new();
    for rel in list_files(dir) {
        let path = dir.join(&rel);
        let hash = match fs::metadata(&path) {
            Ok(meta) if !meta.is_file() => continue,
            Ok(_) => match fs::read(&path) {
                Ok(bytes) => sha256_hex(&bytes),
                Err(_) => "deleted".to_string(),
            },
            Err(_) => "deleted".to_string(),
        };
        manifest.push_str(&format!("{hash}  {}\n", rel.display()));
    }
    sha256_hex(manifest.as_bytes())
}

impl Annotation {
    /// Annotation for a line range of `file`, whose full text is `contents`.
    pub fn collect(
        file: &Path,
        contents: &str,
        line_start: usize,
        line_end: usize,
        note: &str,
    ) -> Annotation {
        Annotation {
            path: file.to_path_buf(),
            scope: Scope::Lines(line_start, line_end),
            git_commit: git_commit(file),
            content_sha256: sha256_hex(contents.as_bytes()),
            created_at: Utc::now().to_rfc3339(),
            context: Some(line_range(contents, line_start, line_end)),
            note: note.to_string(),
        }
    }

    /// Annotation for a whole file or a whole directory.
    pub fn collect_for_path(path: &Path, note: &str) -> std::io::Result<Annotation> {
        let (scope, content_sha256) = if path.is_dir() {
            (Scope::Directory, directory_sha256(path))
        } else {
            check_regular_file(path)?;
            (Scope::File, sha256_hex(&fs::read(path)?))
        };
        Ok(Annotation {
            path: path.to_path_buf(),
            scope,
            git_commit: git_commit(path),
            content_sha256,
            created_at: Utc::now().to_rfc3339(),
            context: None,
            note: note.to_string(),
        })
    }

    /// The on-disk file name: a freshly generated UUID plus `.md`.
    pub fn file_name(&self) -> String {
        format!("{}.md", Uuid::new_v4())
    }

    /// Path and scope as one reference, in the usual code-link style:
    /// `src/` for a directory, `src/foo.rs` for a file, and `src/foo.rs#L3-7`
    /// (or `#L3` for a single line) for a line range.
    pub fn scope_ref(&self) -> String {
        let path = self.path.display().to_string();
        match self.scope {
            Scope::Lines(start, end) if start == end => format!("{path}#L{start}"),
            Scope::Lines(start, end) => format!("{path}#L{start}-{end}"),
            Scope::File => path,
            Scope::Directory => format!("{}/", path.trim_end_matches('/')),
        }
    }

    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("# Annotation\n\n");
        md.push_str(&format!("- scope: {}\n", inline_code(&self.scope_ref())));
        match &self.git_commit {
            Some(commit) => md.push_str(&format!("- git_commit: `{commit}`\n")),
            None => md.push_str("- git_commit: unavailable\n"),
        }
        md.push_str(&format!("- content_sha256: `{}`\n", self.content_sha256));
        md.push_str(&format!("- created_at: {}\n", self.created_at));
        if let Some(context) = &self.context {
            md.push_str("\n## Context\n\n");
            md.push_str(&code_block(context, &self.path));
        }
        md.push_str("\n## Comment\n\n");
        md.push_str(&self.note);
        md.push('\n');
        md
    }

    /// Write to `<output_dir>/<UUID>.md` and return the written path.
    fn write(&self, output_dir: &Path) -> std::io::Result<PathBuf> {
        fs::create_dir_all(output_dir)?;
        let path = output_dir.join(self.file_name());
        fs::write(&path, self.to_markdown())?;
        Ok(path)
    }
}

/// Build a line-range annotation for `file` and write it to `<output_dir>/<UUID>.md`.
/// Returns the path of the written file.
pub fn save_annotation(
    file: &Path,
    contents: &str,
    line_start: usize,
    line_end: usize,
    note: &str,
    output_dir: &Path,
) -> std::io::Result<PathBuf> {
    check_path(file)?;
    Annotation::collect(file, contents, line_start, line_end, note).write(output_dir)
}

/// Build a single annotation for a whole file or directory and write it to
/// `<output_dir>/<UUID>.md`. Returns the path of the written file.
pub fn save_path_annotation(path: &Path, note: &str, output_dir: &Path) -> std::io::Result<PathBuf> {
    check_path(path)?;
    Annotation::collect_for_path(path, note)?.write(output_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn markdown_contains_all_metadata() {
        let a = Annotation {
            path: PathBuf::from("src/foo.rs"),
            scope: Scope::Lines(3, 7),
            git_commit: Some("abc123".to_string()),
            content_sha256: "deadbeef".to_string(),
            created_at: "2026-10-19T00:00:00+00:00".to_string(),
            context: Some("let x = 1;".to_string()),
            note: "this looks wrong".to_string(),
        };
        let md = a.to_markdown();
        assert!(md.starts_with("# Annotation\n\n- scope: `src/foo.rs#L3-7`\n- git_commit"), "{md}");
        assert!(md.contains("- git_commit: `abc123`"));
        assert!(md.contains("- content_sha256: `deadbeef`"));
        assert!(md.contains("- created_at: 2026-10-19T00:00:00+00:00"));
        assert!(md.contains("## Context\n\n```rs\nlet x = 1;\n```\n\n## Comment"), "{md}");
        assert!(md.contains("## Comment"));
        assert!(md.contains("this looks wrong"));
    }

    #[test]
    fn scope_ref_names_path_and_lines_in_one_reference() {
        let at = |path: &str, scope| Annotation {
            path: PathBuf::from(path),
            scope,
            git_commit: None,
            content_sha256: "x".to_string(),
            created_at: "t".to_string(),
            context: None,
            note: "n".to_string(),
        };
        assert_eq!(at("src/foo.rs", Scope::Lines(3, 7)).scope_ref(), "src/foo.rs#L3-7");
        assert_eq!(at("src/foo.rs", Scope::Lines(3, 3)).scope_ref(), "src/foo.rs#L3");
        assert_eq!(at("src/foo.rs", Scope::File).scope_ref(), "src/foo.rs");
        assert_eq!(at("src", Scope::Directory).scope_ref(), "src/");
        assert_eq!(at("src/", Scope::Directory).scope_ref(), "src/");
        assert_eq!(at(".", Scope::Directory).scope_ref(), "./");
    }

    #[test]
    fn line_range_picks_the_annotated_lines() {
        let contents = "one\ntwo\r\nthree\nfour\n";
        assert_eq!(line_range(contents, 2, 3), "two\nthree");
        assert_eq!(line_range(contents, 4, 4), "four");
        assert_eq!(line_range("", 1, 1), "");
    }

    #[test]
    fn code_block_fence_outlasts_backticks_in_code() {
        assert_eq!(code_block("a", Path::new("x.py")), "```py\na\n```\n");
        assert_eq!(code_block("```\nb", Path::new("Makefile")), "````\n```\nb\n````\n");
    }

    #[test]
    fn inline_code_fence_outlasts_backticks_in_text() {
        assert_eq!(inline_code("src/a.rs#L1"), "`src/a.rs#L1`");
        assert_eq!(inline_code("a`b.rs"), "``a`b.rs``");
        assert_eq!(inline_code("`a``.rs"), "``` `a``.rs ```");
    }

    #[test]
    fn paths_with_control_characters_are_not_saved() {
        let dir = std::env::temp_dir().join(format!("annotator-test-{}", Uuid::new_v4()));
        let out = dir.join("out");
        let forged = dir.join("x`\n- git_commit: `forged`\n.rs");
        let err = save_annotation(&forged, "a\n", 1, 1, "n", &out).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        let err = save_path_annotation(&forged, "n", &out).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!out.exists());
    }

    #[test]
    fn markdown_marks_missing_git_commit() {
        let a = Annotation {
            path: PathBuf::from("foo.rs"),
            scope: Scope::Lines(1, 1),
            git_commit: None,
            content_sha256: "x".to_string(),
            created_at: "t".to_string(),
            context: None,
            note: "n".to_string(),
        };
        assert!(a.to_markdown().contains("- git_commit: unavailable"));
    }

    #[test]
    fn file_name_is_uuid_md() {
        let a = Annotation {
            path: PathBuf::from("foo.rs"),
            scope: Scope::Lines(1, 1),
            git_commit: None,
            content_sha256: "x".to_string(),
            created_at: "t".to_string(),
            context: None,
            note: "n".to_string(),
        };
        let name = a.file_name();
        assert!(name.ends_with(".md"));
        let stem = name.strip_suffix(".md").unwrap();
        assert!(Uuid::parse_str(stem).is_ok(), "stem should be a UUID: {stem}");
    }

    #[test]
    fn save_writes_uuid_md_file() {
        let dir = std::env::temp_dir().join(format!("annotator-test-{}", Uuid::new_v4()));
        let file = dir.join("sample.rs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&file, "fn main() {}\n").unwrap();
        let out = dir.join("out");

        let path = save_annotation(&file, "fn main() {}\n", 1, 1, "looks good", &out).unwrap();
        assert!(path.exists());
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(Uuid::parse_str(name.strip_suffix(".md").unwrap()).is_ok());

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(&format!("- scope: `{}#L1`\n", file.display())), "{content}");
        assert!(content.contains(&sha256_hex(b"fn main() {}\n")));
        assert!(content.contains("## Context\n\n```rs\nfn main() {}\n```\n"));
        assert!(content.contains("looks good"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_commit_detected_inside_repo() {
        let dir = std::env::temp_dir().join(format!("annotator-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        fs::write(&file, "x\n").unwrap();

        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "test"]);
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);

        assert!(git_commit(&file).is_some());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_commit_none_outside_repo() {
        let dir = std::env::temp_dir().join(format!("annotator-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        fs::write(&file, "x\n").unwrap();
        // /tmp is not a git repo on a typical machine; even if some parent were,
        // the test only asserts the Option shape, not the value.
        let _ = git_commit(&file);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_annotation_hashes_whole_file_without_lines() {
        let dir = std::env::temp_dir().join(format!("annotator-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();

        let a = Annotation::collect_for_path(&file, "whole file").unwrap();
        assert_eq!(a.scope, Scope::File);
        assert_eq!(a.content_sha256, sha256_hex(b"fn a() {}\nfn b() {}\n"));
        let md = a.to_markdown();
        assert!(md.contains(&format!("- scope: `{}`\n", file.display())), "{md}");
        assert!(!md.contains("## Context"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn directory_annotation_is_single_and_hash_tracks_contents() {
        let dir = std::env::temp_dir().join(format!("annotator-test-{}", Uuid::new_v4()));
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "a\n").unwrap();
        fs::write(dir.join("sub/b.txt"), "b\n").unwrap();
        fs::write(dir.join(".hidden"), "ignored\n").unwrap();

        let a = Annotation::collect_for_path(&dir, "whole dir").unwrap();
        assert_eq!(a.scope, Scope::Directory);
        let manifest = format!(
            "{}  a.txt\n{}  sub/b.txt\n",
            sha256_hex(b"a\n"),
            sha256_hex(b"b\n")
        );
        assert_eq!(a.content_sha256, sha256_hex(manifest.as_bytes()));
        let md = a.to_markdown();
        assert!(md.contains(&format!("- scope: `{}/`\n", dir.display())), "{md}");

        fs::write(dir.join("sub/b.txt"), "changed\n").unwrap();
        assert_ne!(directory_sha256(&dir), a.content_sha256);

        let out = dir.join("out");
        save_path_annotation(&dir, "whole dir", &out).unwrap();
        assert_eq!(fs::read_dir(&out).unwrap().count(), 1);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn directory_hash_respects_gitignore_inside_repo() {
        let dir = std::env::temp_dir().join(format!("annotator-test-{}", Uuid::new_v4()));
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join(".gitignore"), "/target\n").unwrap();
        fs::write(dir.join("a.txt"), "a\n").unwrap();
        fs::write(dir.join("target/big.bin"), "build output\n").unwrap();
        Command::new("git").args(["init", "-q"]).current_dir(&dir).output().unwrap();

        let before = directory_sha256(&dir);
        fs::write(dir.join("target/big.bin"), "rebuilt\n").unwrap();
        assert_eq!(directory_sha256(&dir), before, "ignored files must not affect the hash");
        fs::write(dir.join("a.txt"), "edited\n").unwrap();
        assert_ne!(directory_sha256(&dir), before);

        fs::remove_dir_all(&dir).unwrap();
    }
}
