mod annotation;
mod app;

use std::path::PathBuf;
use std::process::ExitCode;

use annotation::{read_text_file, save_annotation, save_path_annotation};

struct Cli {
    file: Option<PathBuf>,
    path: Option<PathBuf>,
    output_dir: PathBuf,
    lines: Option<(usize, usize)>,
    note: Option<String>,
    user: Option<String>,
}

fn parse_args() -> Result<Cli, String> {
    let mut args = std::env::args().skip(1).peekable();
    let mut file: Option<PathBuf> = None;
    let mut path: Option<PathBuf> = None;
    let mut output_dir = PathBuf::from("annotations");
    let mut lines: Option<(usize, usize)> = None;
    let mut note: Option<String> = None;
    let mut user: Option<String> = None;

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
            "--user" => {
                user = Some(args.next().ok_or("--user requires a value")?);
            }
            "--output-dir" => {
                output_dir = PathBuf::from(args.next().ok_or("--output-dir requires a value")?);
            }
            "--path" => {
                path = Some(PathBuf::from(args.next().ok_or("--path requires a value")?));
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

    if path.is_some() {
        if file.is_some() {
            return Err("give either <file> or --path, not both".into());
        }
        if lines.is_some() {
            return Err("--lines cannot be used with --path (a path annotation covers the whole file or directory)".into());
        }
        if note.is_none() {
            return Err("--path requires --note TEXT".into());
        }
    }
    if note.as_deref().is_some_and(|n| n.trim().is_empty()) {
        return Err("--note must not be empty".into());
    }

    Ok(Cli {
        file,
        path,
        output_dir,
        lines,
        note,
        // A blank name is the same as no name.
        user: user.map(|u| u.trim().to_string()).filter(|u| !u.is_empty()),
    })
}

fn print_help() {
    eprintln!("annotator — annotate code files with review-style comments");
    eprintln!();
    eprintln!("usage:");
    eprintln!("  annotator                              interactive TUI, starting in the file browser");
    eprintln!("  annotator <file>                       interactive TUI, starting in <file> (b to browse, ? for keys)");
    eprintln!("  annotator <file> --lines A-B --note TEXT   save an annotation without the TUI");
    eprintln!("  annotator --path <path> --note TEXT        annotate a whole file or directory");
    eprintln!();
    eprintln!("options:");
    eprintln!("  --lines A-B       line range to annotate (1-based, inclusive)");
    eprintln!("  --note TEXT       comment text (batch mode)");
    eprintln!("  --user NAME       who wrote the comment (batch mode; the TUI asks)");
    eprintln!("  --path PATH       file or directory to annotate as a whole (one annotation)");
    eprintln!("  --output-dir DIR  where to write <UUID>.md files (default: ./annotations)");
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

    // Path mode: one annotation for a whole file or directory
    if let Some(path) = cli.path {
        let note = cli.note.as_deref().unwrap_or_default();
        match save_path_annotation(&path, note.trim(), cli.user.as_deref(), &cli.output_dir) {
            Ok(saved) => {
                println!("saved annotation: {}", saved.display());
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        }
    }

    // File mode: batch save with --lines/--note, otherwise open it in the TUI
    let app = if let Some(file) = cli.file {
        let contents = match read_text_file(&file) {
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
            match save_annotation(&file, &contents, start, end, note.trim(), cli.user.as_deref(), &cli.output_dir) {
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

        app::App::new(&file, &contents, &cli.output_dir)
    } else {
        // No file given: start in the file browser at the current directory.
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        app::App::new_browser(&current_dir, &cli.output_dir)
    };

    match app::run(app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
