// SPDX-License-Identifier: GPL-3.0-or-later
//! lme-shell: the native Wayland daemon for LME Shell (ADR 0002).
//!   lme-shell               run the daemon
//!   lme-shell msg <cmd>...  thin CLI over the daemon socket
//!   lme-shell settings      separate settings window (Tier 3 — stub)
//!   lme-shell --version

mod bar;
mod daemon;
mod ipc;
mod launcher_data;
mod niri;
mod notifications;
mod panels;
mod power;
mod settings;

pub use power::spawn_detached;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("lme-shell {}", daemon::VERSION);
            ExitCode::SUCCESS
        }
        Some("msg") => run_msg(&args[1..]),
        Some("settings") => {
            // Tier 3 (settings window): separate process of this binary.
            println!("lme-shell settings: not implemented in this build (Tier 3)");
            ExitCode::SUCCESS
        }
        Some("daemon") | None => daemon::run(args.iter().any(|a| a == "--dry-run")),
        Some(other) => {
            eprintln!("unknown command: {other}\nusage: lme-shell [daemon [--dry-run]] | msg <command> | settings | --version");
            ExitCode::FAILURE
        }
    }
}

fn run_msg(args: &[String]) -> ExitCode {
    let cmd = match parse_msg(args) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("lme-shell msg: {e}");
            return ExitCode::FAILURE;
        }
    };
    match ipc::send(&cmd) {
        Ok(reply) => {
            if let Some(detail) = &reply.detail {
                println!("{}", reply.message);
                println!(
                    "{}",
                    serde_json::to_string_pretty(detail).unwrap_or_default()
                );
            } else {
                println!("{}", reply.message);
            }
            if reply.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("lme-shell msg: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_msg(args: &[String]) -> Result<ipc::Command, String> {
    match args.first().map(String::as_str) {
        Some("ping") => Ok(ipc::Command::Ping),
        Some("status") => Ok(ipc::Command::Status),
        Some("close") => Ok(ipc::Command::Close),
        Some("refresh") => Ok(ipc::Command::Refresh),
        Some("toggle") => args
            .get(1)
            .map(|panel| ipc::Command::Toggle { panel: panel.clone() })
            .ok_or_else(|| "toggle needs a panel name".into()),
        Some("osd") => {
            let label = args.get(1).ok_or("osd needs a label")?.clone();
            let value: f32 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(-1.0);
            Ok(ipc::Command::Osd { label, value })
        }
        Some("notify") => {
            let summary = args.get(1).ok_or("notify needs a summary")?.clone();
            let body = args.get(2).cloned().unwrap_or_default();
            Ok(ipc::Command::Notify { summary, body })
        }
        Some("dnd") => {
            let minutes: i64 = args
                .get(1)
                .and_then(|v| v.parse().ok())
                .ok_or("dnd needs minutes (-1 | 0 | >0)")?;
            Ok(ipc::Command::Dnd { minutes })
        }
        Some("notifications") => Ok(ipc::Command::Notifications),
        Some("power") => args
            .get(1)
            .map(|action| ipc::Command::Power { action: action.clone() })
            .ok_or_else(|| "power needs an action".into()),
        _ => Err("commands: ping | status | close | refresh | toggle <panel> | osd <label> [value] | notify <summary> [body] | dnd <minutes> | notifications | power <action>".into()),
    }
}
