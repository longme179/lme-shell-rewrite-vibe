//! Argument parsing and mode dispatch, mirroring the Python CLI flags.

use lme_backend::backend::DesktopBackend;
use lme_backend::{actions, clipboard, loopd};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: lme-backend [-h] [--once] [--demo] [--settings]
                   [--restore-wallpaper FILE] [--interval INTERVAL]
                   [--clipboard] [--capture {text,image}] [--directory DIRECTORY]";

struct Arguments {
    once: bool,
    demo: bool,
    settings: bool,
    restore_wallpaper: Option<String>,
    interval: f64,
    clipboard_mode: bool,
    capture: Option<String>,
    directory: Option<PathBuf>,
}

fn fail(message: &str) -> ! {
    eprintln!("{USAGE}\nlme-backend: error: {message}");
    std::process::exit(2);
}

fn parse_args() -> Arguments {
    let mut arguments = Arguments {
        once: false,
        demo: false,
        settings: false,
        restore_wallpaper: None,
        interval: 2.0,
        clipboard_mode: false,
        capture: None,
        directory: None,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        match flag.as_str() {
            "--once" => arguments.once = true,
            "--demo" => arguments.demo = true,
            "--settings" => arguments.settings = true,
            "--clipboard" => arguments.clipboard_mode = true,
            "--capture" => {
                let Some(kind) = raw.next() else {
                    fail("--capture requires a value (text or image)");
                };
                if !matches!(kind.as_str(), "text" | "image") {
                    fail("invalid --capture value: expected 'text' or 'image'");
                }
                arguments.capture = Some(kind);
            }
            "--directory" => {
                let Some(directory) = raw.next() else {
                    fail("--directory requires a value");
                };
                arguments.directory = Some(PathBuf::from(directory));
            }
            "--interval" => {
                let Some(value) = raw.next() else {
                    fail("--interval requires a value");
                };
                arguments.interval = value.parse().unwrap_or_else(|_| {
                    fail(&format!("invalid float value: '{value}'"));
                });
            }
            "--restore-wallpaper" => {
                let Some(file) = raw.next() else {
                    fail("--restore-wallpaper requires a value");
                };
                arguments.restore_wallpaper = Some(file);
            }
            "-h" | "--help" => {
                println!("{USAGE}\n\nThe unprivileged JSON-lines backend for LME Shell.");
                std::process::exit(0);
            }
            other => fail(&format!("unrecognized argument: {other}")),
        }
    }
    if !arguments.interval.is_finite() || arguments.interval < 0.5 {
        fail("--interval must be at least 0.5 seconds");
    }
    if arguments.demo && arguments.restore_wallpaper.is_some() {
        fail("Wallpaper restoration is disabled in demo mode");
    }
    arguments
}

fn main() -> ExitCode {
    let arguments = parse_args();

    if let Some(kind) = arguments.capture {
        // Read at most limit+1 bytes, exactly like the Python capture mode;
        // anything past the limit is rejected by the store.
        let limit = if kind == "text" {
            clipboard::MAX_TEXT
        } else {
            clipboard::MAX_IMAGE
        };
        let mut payload = Vec::new();
        let _ = std::io::stdin()
            .take(limit as u64 + 1)
            .read_to_end(&mut payload);
        let directory = arguments
            .directory
            .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
            .unwrap_or_else(clipboard::history_directory);
        clipboard::capture(&kind, &directory, &payload);
        return ExitCode::SUCCESS;
    }

    if arguments.clipboard_mode {
        let directory = arguments
            .directory
            .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
            .unwrap_or_else(clipboard::history_directory);
        return clipboard::serve(&directory);
    }

    let settings_only = arguments.settings || arguments.restore_wallpaper.is_some();
    let mut backend = DesktopBackend::new(
        arguments.demo,
        settings_only,
        None,
        None,
        PathBuf::from("/proc"),
        PathBuf::from("/sys"),
        !arguments.once,
        true,
        None,
    );
    if let Some(file) = &arguments.restore_wallpaper {
        return match actions::apply_wallpaper(
            &mut backend,
            &serde_json::Value::String(file.clone()),
            false,
            false,
        ) {
            Ok(_) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Wallpaper: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if lme_backend::emit(&backend.snapshot()).is_err() {
        return ExitCode::SUCCESS; // Broken pipe: the shell went away.
    }
    if arguments.once {
        return ExitCode::SUCCESS;
    }
    loopd::run(&mut backend, arguments.interval);
    ExitCode::SUCCESS
}
