//! Subprocess runner mirroring `CommandRunner` in desktop_backend.py:
//! capability discovery cached once, `LC_ALL=C` environment, timeouts,
//! no shell involved anywhere.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

pub const TOOLS: &[&str] = &[
    "niri",
    "nmcli",
    "bluetoothctl",
    "wpctl",
    "brightnessctl",
    "busctl",
    "powerprofilesctl",
    "gio",
    "swaylock",
    "systemctl",
    "awww",
    "awww-daemon",
    "pkexec",
    "nm-connection-editor",
    "blueman-manager",
    "pavucontrol",
    "rfkill",
];

#[derive(Debug)]
pub struct CommandError(pub String);

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CommandError {}

impl From<CommandError> for String {
    fn from(error: CommandError) -> Self {
        error.0
    }
}

#[derive(Default)]
pub struct CommandRunner {
    commands: HashMap<&'static str, Option<PathBuf>>,
}

impl CommandRunner {
    pub fn new() -> Self {
        let mut commands = HashMap::new();
        for name in TOOLS {
            commands.insert(*name, crate::pyutil::which(name));
        }
        Self { commands }
    }

    pub fn available(&self, name: &str) -> bool {
        self.commands.get(name).is_some_and(|path| path.is_some())
    }

    pub fn executable(&self, name: &str) -> Result<&PathBuf, CommandError> {
        self.commands
            .get(name)
            .and_then(|path| path.as_ref())
            .ok_or_else(|| CommandError(format!("Required command is unavailable: {name}")))
    }

    /// `run`: capture stdout, enforce timeout, map failures to `CommandError`
    /// with the same message shape as the Python runner.
    pub fn run(&self, name: &str, args: &[&str]) -> Result<String, CommandError> {
        self.run_with_timeout(name, args, Duration::from_secs(3))
    }

    pub fn run_with_timeout(
        &self,
        name: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<String, CommandError> {
        let mut command = Command::new(self.executable(name)?);
        command
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            CommandError(format!("Unable to run {name}: {}", error_kind(&error)))
        })?;
        // Read concurrently with waiting so a chatty child cannot deadlock
        // on a full pipe while we block in wait().
        let mut stdout_pipe = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let reader = std::thread::scope(|scope| {
            let out_handle = scope.spawn(|| stdout_pipe.read_to_end(&mut stdout));
            let err_handle = scope.spawn(|| stderr_pipe.read_to_end(&mut stderr));
            let wait_result = {
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if let Ok(Some(status)) = child.try_wait() {
                        break Ok(status);
                    }
                    if std::time::Instant::now() >= deadline {
                        break Err(());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            };
            match wait_result {
                Ok(status) => {
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    Ok(status)
                }
                Err(()) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    Err(CommandError(format!("{name} timed out")))
                }
            }
        });
        let status = reader?;
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_string();
        if !status.success() {
            let code = status.code().unwrap_or(-1);
            let detail = {
                let candidate = if stderr.is_empty() { &stdout } else { &stderr };
                let trimmed = text(candidate);
                if trimmed.is_empty() {
                    text(&stdout)
                } else {
                    trimmed
                }
            };
            let detail: String = detail.chars().take(350).collect();
            let suffix = if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            };
            return Err(CommandError(format!("{name} failed ({code}){suffix}")));
        }
        Ok(text(&stdout))
    }

    /// `spawn`: detached child with all standard streams silenced.
    pub fn spawn(&self, argv: &[&str]) -> Result<(), CommandError> {
        Command::new(argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map(|_| ())
            .map_err(|error| {
                CommandError(format!(
                    "Unable to launch {}: {}",
                    std::path::Path::new(argv[0])
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    error_kind(&error)
                ))
            })
    }
}

fn error_kind(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let message = std::io::Error::from_raw_os_error(code)
                .to_string()
                .split(" (os error")
                .next()
                .unwrap_or("")
                .to_string();
            message
        }
        None => error.to_string(),
    }
}
