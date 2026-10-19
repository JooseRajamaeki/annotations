mod annotation;
mod app;

use std::path::PathBuf;
use std::process::ExitCode;

use annotation::save_annotation;

struct Cli {
    file: PathBuf,
    output_dir: PathBuf,
    lines: Option<(usize, usize)>,
    note: Option<String>,
}

fn parse_args() -> Result<Cli, String> {
    let mut args = std::env::args().skip(1).peekable();
    let mut file: Option<PathBuf> = None;
    let mut output_dir = PathBuf::from("annotations");
    let mut lines: Option<(usize, usize)> = None;
    let mut note: Option<String> = None;

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
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other if file.is_none() && !other.starts_with('-') => {
                file = Some(PathBuf::from(other));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let file = file.ok_or("usage: annotator <file> [--lines A-B --note TEXT] [--output-dir DIR]")?;
    Ok(Cli {
        file,
        output_dir,
        lines,
        note,
    })
}

fn print_help() {
    eprintln!("annotator — annotate code files with review-style comments");
    eprintln!();
    eprintln!("usage:");
    eprintln!("  annotator <file>                          interactive TUI");
    eprintln!("  annotator <file> --lines A-B --note TEXT  save an annotation without the TUI");
    eprintln!();
    eprintln!("options:");
    eprintln!("  --lines A-B       line range to annotate (1-based, inclusive)");
    eprintln!("  --note TEXT       comment text (batch mode)");
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

    let contents = match std::fs::read_to_string(&cli.file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", cli.file.display());
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
        match save_annotation(&cli.file, &contents, start, end, &note, &cli.output_dir) {
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

    match app::run(&cli.file, &contents, &cli.output_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
