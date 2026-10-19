mod annotation;
mod app;

use std::path::PathBuf;
use std::process::ExitCode;

use annotation::{save_annotation, save_path_annotation, HashScope};

struct Cli {
    file: Option<PathBuf>,
    path: Option<PathBuf>,
    output_dir: PathBuf,
    lines: Option<(usize, usize)>,
    note: Option<String>,
    hash_scope: HashScope,
}

fn parse_args() -> Result<Cli, String> {
    let mut args = std::env::args().skip(1).peekable();
    let mut file: Option<PathBuf> = None;
    let mut path: Option<PathBuf> = None;
    let mut output_dir = PathBuf::from("annotations");
    let mut lines: Option<(usize, usize)> = None;
    let mut note: Option<String> = None;
    let mut hash_scope = HashScope::File;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lines" => {
                let v = args.next().ok_or("--lines requires a value like 3-7")?;
                let (a, b) = v
                    .split_once('-')
                    .ok_or("expected --lines START-END")?;
                let start: usize = a.parse().map_err(|_| "invalid line number")?;
                let end: usize = b.parse().map_err(|_| "invalid line number")?;
                if start == 0 || end < start {
                    return Err("invalid line range".into());
                }
                lines = Some((start, end));
            }
            "--note" => {
                note = Some(args.next().ok_or("--note requires a value")?);
            }
            "--output-dir" => {
                output_dir = PathBuf::from(args.next().ok_or("--output-dir requires a value")?);
            }
            "--path" => {
                path = Some(PathBuf::from(args.next().ok_or("--path requires a value")?));
            }
            "--hash-scope" => {
                let scope = args.next().ok_or("--hash-scope requires a value (file or range)")?;
                hash_scope = match scope.as_str() {
                    "file" => HashScope::File,
                    "range" | "line" | "lines" => HashScope::LineRange,
                    other => return Err(format!("invalid hash scope: {other} (expected file or range)")),
                };
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other if file.is_none() && path.is_none() && !other.starts_with('-') => {
                file = Some(PathBuf::from(other));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    // Validate that we have either a file or a path
    if file.is_none() && path.is_none() {
        return Err("usage: annotator <file> [--lines A-B --note TEXT] [--output-dir DIR] [--hash-scope file|range]".into());
    }
    
    Ok(Cli {
        file,
        path,
        output_dir,
        lines,
        note,
        hash_scope,
    })
}

fn print_help() {
    eprintln!("annotator — annotate code files with review-style comments");
    eprintln!();
    eprintln!("usage:");
    eprintln!("  annotator                              interactive file browser");
    eprintln!("  annotator <file>                       interactive TUI");
    eprintln!("  annotator <file> --lines A-B --note TEXT   save an annotation without the TUI");
    eprintln!("  annotator --path <path> --note TEXT        annotate all files in a path (recursive)");
    eprintln!();
    eprintln!("options:");
    eprintln!("  --lines A-B       line range to annotate (1-based, inclusive)");
    eprintln!("  --note TEXT       comment text (batch mode)");
    eprintln!("  --path PATH       path or directory to annotate (recursive if directory)");
    eprintln!("  --output-dir DIR  where to write <UUID>.md files (default: ./annotations)");
    eprintln!("  --hash-scope SCOPE hash scope: file (default) or range");
}

fn main() -> ExitCode {
    let cli = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            print_help();
            return ExitCode::from(2);
        }
    };

    // Handle path mode (recursive annotation of directories)
    if let Some(path) = cli.path {
        let note = cli.note.as_deref().unwrap_or("");
        match save_path_annotation(&path, note, &cli.output_dir, cli.hash_scope) {
            Ok(paths) => {
                for p in &paths {
                    println!("saved annotation: {}", p.display());
                }
                if paths.is_empty() {
                    eprintln!("warning: no files found in path {}", path.display());
                }
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        }
    }

    // Handle file mode
    if let Some(file) = cli.file {
        let contents = match std::fs::read_to_string(&file) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", file.display());
                return ExitCode::from(1);
            }
        };

        let total_lines = contents.lines().count();
        if let (Some((start, end)), Some(note)) = (cli.lines, cli.note.clone()) {
            if start > total_lines {
                eprintln!("error: line range starts beyond file length ({} lines)", total_lines);
                return ExitCode::from(1);
            }
            let end = end.min(total_lines);
            match save_annotation(&file, &contents, start, end, &note, &cli.output_dir) {
                Ok(path) => {
                    println!("saved annotation: {}", path.display());
                    return ExitCode::SUCCESS;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            }
        }

        match app::run(&file, &contents, &cli.output_dir) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        }
    } else {
        // No file or path specified - launch file browser
        // Use current directory as starting point
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match app::run_browser(&current_dir, &cli.output_dir) {
            Ok(Some(selected_file)) => {
                // File selected from browser - load it
                let contents = match std::fs::read_to_string(&selected_file) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("error: cannot read {}: {e}", selected_file.display());
                        return ExitCode::from(1);
                    }
                };
                match app::run(&selected_file, &contents, &cli.output_dir) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("error: {e}");
                        ExitCode::from(1)
                    }
                }
            }
            Ok(None) => ExitCode::SUCCESS, // User quit without selecting
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        }
    }
}
