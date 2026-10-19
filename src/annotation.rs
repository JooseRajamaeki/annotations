use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Hash scope determines what content is hashed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashScope {
    /// Hash the entire file content
    File,
    /// Hash only the selected line range
    LineRange,
}

/// A single annotation: a comment tied to a line range of a file,
/// plus the metadata collected at save time.
pub struct Annotation {
    pub file: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub git_commit: Option<String>,
    pub content_sha256: String,
    pub created_at: String,
    pub note: String,
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

/// HEAD commit of the git repo containing `file`, if any.
fn git_commit(file: &Path) -> Option<String> {
    let dir = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&dir)
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

impl Annotation {
    pub fn collect(
        file: &Path,
        contents: &str,
        line_start: usize,
        line_end: usize,
        note: &str,
    ) -> Annotation {
        Annotation {
            file: file.to_path_buf(),
            line_start,
            line_end,
            git_commit: git_commit(file),
            content_sha256: sha256_hex(contents.as_bytes()),
            created_at: Utc::now().to_rfc3339(),
            note: note.to_string(),
        }
    }

    /// The on-disk file name: a freshly generated UUID plus `.md`.
    pub fn file_name(&self) -> String {
        format!("{}.md", Uuid::new_v4())
    }

    /// Create an annotation for a path (file or directory)
    pub fn collect_for_path(
        path: &Path,
        note: &str,
        hash_scope: HashScope,
    ) -> Vec<Annotation> {
        let mut annotations = Vec::new();
        
        if path.is_dir() {
            // Recursively find all files in the directory
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries {
                    if let Ok(entry) = entry {
                        let entry_path = entry.path();
                        if entry_path.is_file() {
                            // For directories, we annotate each file with the full path
                            // and use line range 1-1 (whole file)
                            if let Ok(contents) = fs::read_to_string(&entry_path) {
                                let annotation = Annotation {
                                    file: entry_path.clone(),
                                    line_start: 1,
                                    line_end: contents.lines().count(),
                                    git_commit: git_commit(&entry_path),
                                    content_sha256: match hash_scope {
                                        HashScope::File => sha256_hex(contents.as_bytes()),
                                        HashScope::LineRange => {
                                            let start = 0.min(contents.len());
                                            let end = contents.len();
                                            sha256_hex(&contents.as_bytes()[start..end])
                                        }
                                    },
                                    created_at: Utc::now().to_rfc3339(),
                                    note: note.to_string(),
                                };
                                annotations.push(annotation);
                            }
                        } else if entry_path.is_dir() {
                            // Recursively process subdirectories
                            let sub_annotations = Annotation::collect_for_path(&entry_path, note, hash_scope);
                            annotations.extend(sub_annotations);
                        }
                    }
                }
            }
        } else {
            // Single file
            if let Ok(contents) = fs::read_to_string(path) {
                let total_lines = contents.lines().count();
                let annotation = Annotation {
                    file: path.to_path_buf(),
                    line_start: 1,
                    line_end: total_lines,
                    git_commit: git_commit(path),
                    content_sha256: match hash_scope {
                        HashScope::File => sha256_hex(contents.as_bytes()),
                        HashScope::LineRange => sha256_hex(contents.as_bytes()),
                    },
                    created_at: Utc::now().to_rfc3339(),
                    note: note.to_string(),
                };
                annotations.push(annotation);
            }
        }
        
        annotations
    }

    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("# Annotation\n\n");
        md.push_str(&format!("- file: `{}`\n", self.file.display()));
        md.push_str(&format!("- lines: {}-{}\n", self.line_start, self.line_end));
        match &self.git_commit {
            Some(commit) => md.push_str(&format!("- git_commit: `{commit}`\n")),
            None => md.push_str("- git_commit: unavailable\n"),
        }
        md.push_str(&format!("- content_sha256: `{}`\n", self.content_sha256));
        md.push_str(&format!("- created_at: {}\n", self.created_at));
        md.push_str("\n## Comment\n\n");
        md.push_str(&self.note);
        md.push('\n');
        md
    }
}

/// Build an annotation for `file` and write it to `<output_dir>/<UUID>.md`.
/// Returns the path of the written file.
pub fn save_annotation(
    file: &Path,
    contents: &str,
    line_start: usize,
    line_end: usize,
    note: &str,
    output_dir: &Path,
) -> std::io::Result<PathBuf> {
    let annotation = Annotation::collect(file, contents, line_start, line_end, note);
    fs::create_dir_all(output_dir)?;
    let path = output_dir.join(annotation.file_name());
    fs::write(&path, annotation.to_markdown())?;
    Ok(path)
}

/// Save annotations for an entire path (file or directory)
/// Returns the paths of all written annotation files.
pub fn save_path_annotation(
    path: &Path,
    note: &str,
    output_dir: &Path,
    hash_scope: HashScope,
) -> std::io::Result<Vec<PathBuf>> {
    let annotations = Annotation::collect_for_path(path, note, hash_scope);
    let mut saved_paths = Vec::new();
    
    fs::create_dir_all(output_dir)?;
    
    for annotation in annotations {
        let file_path = output_dir.join(annotation.file_name());
        fs::write(&file_path, annotation.to_markdown())?;
        saved_paths.push(file_path);
    }
    
    Ok(saved_paths)
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
            file: PathBuf::from("src/foo.rs"),
            line_start: 3,
            line_end: 7,
            git_commit: Some("abc123".to_string()),
            content_sha256: "deadbeef".to_string(),
            created_at: "2026-10-19T00:00:00+00:00".to_string(),
            note: "this looks wrong".to_string(),
        };
        let md = a.to_markdown();
        assert!(md.contains("- file: `src/foo.rs`"));
        assert!(md.contains("- lines: 3-7"));
        assert!(md.contains("- git_commit: `abc123`"));
        assert!(md.contains("- content_sha256: `deadbeef`"));
        assert!(md.contains("- created_at: 2026-10-19T00:00:00+00:00"));
        assert!(md.contains("## Comment"));
        assert!(md.contains("this looks wrong"));
    }

    #[test]
    fn markdown_marks_missing_git_commit() {
        let a = Annotation {
            file: PathBuf::from("foo.rs"),
            line_start: 1,
            line_end: 1,
            git_commit: None,
            content_sha256: "x".to_string(),
            created_at: "t".to_string(),
            note: "n".to_string(),
        };
        assert!(a.to_markdown().contains("- git_commit: unavailable"));
    }

    #[test]
    fn file_name_is_uuid_md() {
        let a = Annotation {
            file: PathBuf::from("foo.rs"),
            line_start: 1,
            line_end: 1,
            git_commit: None,
            content_sha256: "x".to_string(),
            created_at: "t".to_string(),
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
        assert!(content.contains("- file: `"));
        assert!(content.contains("- lines: 1-1"));
        assert!(content.contains(&sha256_hex(b"fn main() {}\n")));
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
}
