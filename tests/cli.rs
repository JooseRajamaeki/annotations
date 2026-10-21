//! End-to-end tests: run the real `annotator` binary in temporary directories
//! and git repos, then check the annotation files it writes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

/// A temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        let dir = std::env::temp_dir().join(format!("annotator-cli-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A git repo with everything currently in it committed.
fn commit_all(dir: &Path) -> String {
    if !dir.join(".git").exists() {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "test"]);
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "commit"]);
    git(dir, &["rev-parse", "HEAD"])
}

fn annotator(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_annotator"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

/// Run a command that should save one annotation; return its Markdown.
fn annotate(cwd: &Path, args: &[&str]) -> String {
    let out = annotator(cwd, args);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    let saved = stdout
        .trim()
        .strip_prefix("saved annotation: ")
        .unwrap_or_else(|| panic!("unexpected stdout: {stdout}"));
    let saved = cwd.join(saved);
    let name = saved.file_name().unwrap().to_string_lossy().to_string();
    assert!(uuid::Uuid::parse_str(name.strip_suffix(".md").unwrap()).is_ok(), "not <UUID>.md: {name}");
    fs::read_to_string(saved).unwrap()
}

/// Value of a `- key: value` metadata line, with backticks stripped.
fn field<'a>(md: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("- {key}: ");
    md.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .map(|v| v.trim_matches('`'))
}

fn comment(md: &str) -> &str {
    md.split_once("## Comment\n\n").unwrap().1.trim_end()
}

#[test]
fn help_exits_successfully() {
    let dir = TempDir::new();
    let out = annotator(dir.path(), &["--help"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage:"));
}

#[test]
fn line_annotation_records_all_metadata() {
    let repo = TempDir::new();
    let file = repo.write("src/lib.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
    let head = commit_all(repo.path());

    let md = annotate(repo.path(), &["src/lib.rs", "--lines", "2-3", "--note", "rename these"]);
    assert_eq!(field(&md, "scope"), Some("src/lib.rs#L2-3"));
    // Scope replaces the separate file/path/lines fields.
    for old in ["file", "path", "lines"] {
        assert_eq!(field(&md, old), None, "{md}");
    }
    assert_eq!(field(&md, "git_commit"), Some(head.as_str()));
    // The hash covers the whole file, not just the selected lines.
    assert_eq!(field(&md, "content_sha256"), Some(sha256_hex(&fs::read(file).unwrap()).as_str()));
    let created = field(&md, "created_at").unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(created).is_ok(), "{created}");
    assert_eq!(comment(&md), "rename these");
    assert!(md.contains("## Context\n\n```rs\nfn b() {}\nfn c() {}\n```\n"), "{md}");
}

#[test]
fn hash_reveals_uncommitted_changes() {
    let repo = TempDir::new();
    repo.write("a.rs", "fn a() {}\n");
    let head = commit_all(repo.path());
    let clean = annotate(repo.path(), &["a.rs", "--lines", "1-1", "--note", "x"]);

    repo.write("a.rs", "fn a() { todo!() }\n");
    let dirty = annotate(repo.path(), &["a.rs", "--lines", "1-1", "--note", "x"]);

    assert_eq!(field(&clean, "git_commit"), Some(head.as_str()));
    assert_eq!(field(&dirty, "git_commit"), Some(head.as_str()));
    assert_ne!(field(&clean, "content_sha256"), field(&dirty, "content_sha256"));
}

#[test]
fn git_commit_unavailable_outside_repo() {
    let dir = TempDir::new();
    dir.write("a.rs", "x\n");
    // Skip if the temp dir happens to live inside a git repo.
    if Command::new("git").args(["rev-parse"]).current_dir(dir.path()).output().unwrap().status.success() {
        return;
    }
    let md = annotate(dir.path(), &["a.rs", "--lines", "1-1", "--note", "x"]);
    assert_eq!(field(&md, "git_commit"), Some("unavailable"));
}

#[test]
fn line_range_end_is_clamped_to_file_length() {
    let dir = TempDir::new();
    dir.write("a.rs", "1\n2\n3\n");
    let md = annotate(dir.path(), &["a.rs", "--lines", "2-99", "--note", "x"]);
    assert_eq!(field(&md, "scope"), Some("a.rs#L2-3"));
}

#[test]
fn note_is_trimmed_and_multiline_preserved() {
    let dir = TempDir::new();
    dir.write("a.rs", "1\n");
    let md = annotate(dir.path(), &["a.rs", "--lines", "1-1", "--note", "  first\nsecond  "]);
    assert_eq!(comment(&md), "first\nsecond");
}

#[test]
fn output_dir_defaults_to_annotations() {
    let dir = TempDir::new();
    dir.write("a.rs", "1\n");
    annotate(dir.path(), &["a.rs", "--lines", "1-1", "--note", "x"]);
    assert_eq!(fs::read_dir(dir.path().join("annotations")).unwrap().count(), 1);
}

#[test]
fn file_path_annotation_covers_whole_file() {
    let repo = TempDir::new();
    let file = repo.write("a.rs", "fn a() {}\n");
    let head = commit_all(repo.path());

    let md = annotate(repo.path(), &["--path", "a.rs", "--note", "whole file"]);
    assert_eq!(field(&md, "scope"), Some("a.rs"));
    assert_eq!(field(&md, "git_commit"), Some(head.as_str()));
    assert_eq!(field(&md, "content_sha256"), Some(sha256_hex(&fs::read(file).unwrap()).as_str()));
    assert_eq!(comment(&md), "whole file");
}

#[test]
fn directory_annotation_is_one_file_with_manifest_hash() {
    let repo = TempDir::new();
    repo.write(".gitignore", "/target\n");
    repo.write("src/a.rs", "fn a() {}\n");
    repo.write("src/nested/b.rs", "fn b() {}\n");
    repo.write("target/out.bin", "build output\n");
    let head = commit_all(repo.path());
    let out = TempDir::new();
    let out_dir = out.path().to_str().unwrap();

    let md = annotate(repo.path(), &["--path", ".", "--note", "whole repo", "--output-dir", out_dir]);
    assert_eq!(fs::read_dir(out.path()).unwrap().count(), 1, "one annotation, not one per file");
    assert_eq!(field(&md, "scope"), Some("./"));
    assert_eq!(field(&md, "git_commit"), Some(head.as_str()));

    // Same as `git ls-files -co --exclude-standard | sort | xargs sha256sum | sha256sum`.
    let manifest: String = [".gitignore", "src/a.rs", "src/nested/b.rs"]
        .iter()
        .map(|f| format!("{}  {f}\n", sha256_hex(&fs::read(repo.path().join(f)).unwrap())))
        .collect();
    let hash = field(&md, "content_sha256").unwrap().to_string();
    assert_eq!(hash, sha256_hex(manifest.as_bytes()));

    let rehash = |note: &str| {
        let md = annotate(repo.path(), &["--path", ".", "--note", note, "--output-dir", out_dir]);
        field(&md, "content_sha256").unwrap().to_string()
    };
    repo.write("target/out.bin", "rebuilt\n");
    assert_eq!(rehash("ignored change"), hash, "gitignored files do not count");
    repo.write("src/new.rs", "fn new() {}\n");
    let with_untracked = rehash("untracked file");
    assert_ne!(with_untracked, hash, "untracked files count");
    fs::remove_file(repo.path().join("src/a.rs")).unwrap();
    assert_ne!(rehash("deleted file"), with_untracked, "deleted files count");
}

#[test]
fn subdirectory_annotation_hashes_only_that_directory() {
    let repo = TempDir::new();
    repo.write("src/a.rs", "fn a() {}\n");
    repo.write("docs/readme.md", "hi\n");
    commit_all(repo.path());
    let out = TempDir::new();
    let args = ["--path", "src", "--note", "x", "--output-dir", out.path().to_str().unwrap()];

    let before = annotate(repo.path(), &args);
    repo.write("docs/readme.md", "changed\n");
    let after = annotate(repo.path(), &args);
    assert_eq!(field(&before, "content_sha256"), field(&after, "content_sha256"));
}

#[test]
fn invalid_arguments_are_rejected_without_writing() {
    let dir = TempDir::new();
    dir.write("a.rs", "1\n2\n");
    let cases: &[(&[&str], &str)] = &[
        (&["--path", "a.rs"], "--path requires --note"),
        (&["--path", "a.rs", "--note", "  "], "--note must not be empty"),
        (&["a.rs", "--lines", "1-1", "--note", ""], "--note must not be empty"),
        (&["--path", "a.rs", "--lines", "1-1", "--note", "x"], "--lines cannot be used with --path"),
        (&["--path", "a.rs", "b.rs", "--note", "x"], "unknown argument: b.rs"),
        (&["a.rs", "--hash-scope", "range"], "unknown argument: --hash-scope"),
        (&["a.rs", "--lines", "0-1", "--note", "x"], "invalid line range"),
        (&["a.rs", "--lines", "2-1", "--note", "x"], "invalid line range"),
        (&["a.rs", "--lines", "x-1", "--note", "x"], "invalid line number"),
        (&["a.rs", "--lines", "3", "--note", "x"], "expected --lines START-END"),
        (&["a.rs", "--bogus"], "unknown argument: --bogus"),
    ];
    for (args, expected) in cases {
        let out = annotator(dir.path(), args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(expected), "{args:?}: expected {expected:?} in {stderr}");
    }
    assert!(!dir.path().join("annotations").exists());
}

#[test]
fn runtime_errors_exit_with_one() {
    let dir = TempDir::new();
    dir.write("a.rs", "1\n2\n");
    let cases: &[(&[&str], &str)] = &[
        (&["a.rs", "--lines", "5-6", "--note", "x"], "beyond file length"),
        (&["missing.rs", "--lines", "1-1", "--note", "x"], "cannot read missing.rs"),
        (&["--path", "missing", "--note", "x"], "error:"),
    ];
    for (args, expected) in cases {
        let out = annotator(dir.path(), args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(stderr.contains(expected), "{args:?}: expected {expected:?} in {stderr}");
    }
    assert!(!dir.path().join("annotations").exists());
}

#[test]
fn file_names_cannot_forge_metadata() {
    let dir = TempDir::new();
    dir.write("a`b.rs", "x\n");
    let md = annotate(dir.path(), &["a`b.rs", "--lines", "1-1", "--note", "x"]);
    assert!(md.contains("- scope: ``a`b.rs#L1``\n"), "{md}");

    let forged = "x`\n- git_commit: `forged`\n#.rs";
    dir.write(forged, "x\n");
    let out = annotator(dir.path(), &[forged, "--lines", "1-1", "--note", "x"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("control characters"), "{stderr}");
    assert_eq!(fs::read_dir(dir.path().join("annotations")).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn fifos_are_refused_or_skipped_instead_of_blocking() {
    let repo = TempDir::new();
    repo.write("a.rs", "fn a() {}\n");
    let made = Command::new("mkfifo").arg(repo.path().join("fifo")).status().unwrap();
    assert!(made.success());
    std::os::unix::fs::symlink("fifo", repo.path().join("link")).unwrap();
    git(repo.path(), &["init", "-q"]);

    for args in [&["fifo", "--lines", "1-1", "--note", "x"][..], &["--path", "link", "--note", "x"]] {
        let out = annotator(repo.path(), args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(stderr.contains("not a regular file"), "{args:?}: {stderr}");
    }

    let md = annotate(repo.path(), &["--path", ".", "--note", "x"]);
    let manifest = format!("{}  a.rs\n", sha256_hex(b"fn a() {}\n"));
    assert_eq!(field(&md, "content_sha256"), Some(sha256_hex(manifest.as_bytes()).as_str()));
}
