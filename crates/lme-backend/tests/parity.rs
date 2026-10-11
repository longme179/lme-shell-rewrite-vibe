//! Golden JSON parity: the Rust `--once` snapshot must equal the Python
//! backend's snapshot (live comparison when python3 is available, plus a
//! committed golden file pinning the normalized shape), in live, demo and
//! settings modes — with identical subprocess call sequences.

mod common;

use common::{capture, normalize, python3, repo_root, Fixture};
use serde_json::Value;

const RUST: &str = env!("CARGO_BIN_EXE_lme-backend");

fn first_line(text: &str) -> Value {
    let line = text.lines().next().expect("one snapshot line");
    serde_json::from_str(line).expect("snapshot is valid JSON")
}

fn parity_case(name: &str, args: &[&str], golden: Option<&str>) {
    let fixture = Fixture::new(name);
    let python_path = common::python_tree().join("scripts/desktop_backend.py");

    fixture.reset_log();
    let rust_out = capture(&fixture, RUST.as_ref(), args);
    let rust_calls = fixture.log_lines();

    if let Some(python) = python3() {
        fixture.reset_log();
        let mut python_args = vec![python_path.to_string_lossy().as_ref().to_string()];
        python_args.extend(args.iter().map(|argument| argument.to_string()));
        let borrowed: Vec<&str> = python_args.iter().map(String::as_str).collect();
        let python_out = capture(&fixture, &python, &borrowed);
        let python_calls = fixture.log_lines();
        let mut left = first_line(&python_out);
        let mut right = first_line(&rust_out);
        normalize(&mut left, &fixture.root);
        normalize(&mut right, &fixture.root);
        assert_eq!(
            left, right,
            "Python and Rust snapshots differ in {name} mode\npython: {left}\nrust:   {right}"
        );
        // Byte-level check after normalization pins key order too.
        assert_eq!(
            serde_json::to_string(&left).unwrap(),
            serde_json::to_string(&right).unwrap(),
            "normalized serialization differs in {name} mode"
        );
        // Identical subprocess usage (same tools, same order).
        assert_eq!(
            python_calls, rust_calls,
            "subprocess call sequences differ in {name} mode"
        );
    }

    if let Some(golden_name) = golden {
        let golden_path = repo_root()
            .join("lme-backend/tests/fixtures")
            .join(golden_name);
        let mut actual = first_line(&rust_out);
        normalize(&mut actual, &fixture.root);
        let golden: Value = serde_json::from_str(
            &std::fs::read_to_string(&golden_path).expect("golden fixture present"),
        )
        .expect("golden JSON");
        assert_eq!(
            actual, golden,
            "Rust snapshot drifted from the committed golden {golden_name}"
        );
    }
}

#[test]
#[ignore = "upstream drift: snapshot grew 9 top-level fields (see docs/UPSTREAM_SYNC.md); lme-backend implements the Phase 0 frozen contract"]
fn once_live_mode_matches_python() {
    parity_case("once", &["--once"], Some("golden-once.json"));
}

#[test]
#[ignore = "upstream drift: snapshot grew 9 top-level fields (see docs/UPSTREAM_SYNC.md); lme-backend implements the Phase 0 frozen contract"]
fn once_demo_mode_matches_python() {
    parity_case("demo", &["--demo", "--once"], Some("golden-demo.json"));
}

#[test]
#[ignore = "upstream drift: snapshot grew 9 top-level fields (see docs/UPSTREAM_SYNC.md); lme-backend implements the Phase 0 frozen contract"]
fn once_settings_mode_matches_python() {
    parity_case(
        "settings",
        &["--settings", "--once"],
        Some("golden-settings.json"),
    );
}

#[test]
fn interval_validation_matches_python() {
    crate::require_legacy_reference!();
    let fixture = Fixture::new("interval");
    let run = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .env_clear()
            .envs(fixture.env())
            .output()
            .expect("spawn")
    };
    let rust = run(RUST, &["--interval", "0.1"]);
    assert_eq!(
        rust.status.code(),
        Some(2),
        "rust must reject tiny intervals"
    );
    assert!(String::from_utf8_lossy(&rust.stderr).contains("at least 0.5"));
    if let Some(python) = python3() {
        let output = run(
            python.to_string_lossy().as_ref(),
            &[
                common::python_tree()
                    .join("scripts/desktop_backend.py")
                    .to_string_lossy()
                    .as_ref(),
                "--interval",
                "0.1",
            ],
        );
        assert_eq!(output.status.code(), Some(2));
    }
    // 0.5 itself is accepted by both.
    let rust = run(RUST, &["--interval", "0.5", "--once"]);
    assert!(rust.status.success());
}

#[test]
fn snapshot_covers_expected_surface() {
    let fixture = Fixture::new("surface");
    let out = capture(&fixture, RUST.as_ref(), &["--once"]);
    let snapshot = first_line(&out);
    // Apps: alpha (localized), system-tool, terminal-app, zeta — hidden,
    // kde-only and broken filtered; user alpha wins over the system one.
    let apps: Vec<&str> = snapshot["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|app| app["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        apps,
        vec!["Alpha US", "System Tool", "Terminal App", "Zeta"]
    );
    assert_eq!(snapshot["apps"][0]["comment"], "US comment");
    assert_eq!(snapshot["apps"][0]["genericName"], "Editor US");
    assert_eq!(
        snapshot["apps"][0]["keywords"],
        serde_json::json!(["text", "editor"])
    );
    assert!(snapshot["apps"]
        .as_array()
        .unwrap()
        .iter()
        .all(|app| app.get("argv").is_none()));
    // niri via stubs: sorted workspaces, raw window pass-through.
    assert_eq!(snapshot["niri"]["available"], serde_json::json!(true));
    assert_eq!(snapshot["niri"]["workspaces"].as_array().unwrap().len(), 2);
    assert_eq!(
        snapshot["niri"]["windows"][0]["title"],
        serde_json::json!("Firefox")
    );
    // Probes via stubs.
    assert_eq!(snapshot["system"]["audio"]["volume"], serde_json::json!(45));
    assert_eq!(
        snapshot["system"]["brightness"]["percent"],
        serde_json::json!(45)
    );
    assert_eq!(
        snapshot["system"]["powerProfile"],
        serde_json::json!("balanced")
    );
    assert_eq!(
        snapshot["system"]["network"]["ssid"],
        serde_json::json!("HomeNet")
    );
    assert_eq!(
        snapshot["system"]["bluetooth"]["devices"][0]["name"],
        serde_json::json!("Headphones")
    );
    // First CPU sample is null by contract; recording is inactive.
    assert_eq!(snapshot["system"]["cpu"], serde_json::Value::Null);
    assert_eq!(
        snapshot["system"]["recording"],
        serde_json::json!({"active": false})
    );
    // Preferences merged with defaults; invalid saved keys skipped.
    assert_eq!(snapshot["preferences"]["opacity"], serde_json::json!(0.85));
    assert_eq!(
        snapshot["preferences"]["accent"],
        serde_json::json!("#bd93f9")
    );
    assert_eq!(
        snapshot["preferences"]["barFontSize"],
        serde_json::json!(14)
    );
    assert_eq!(snapshot["preferences"]["language"], serde_json::json!("en"));
    assert!(snapshot["preferences"].get("unknownKey").is_none());
    // lastResult starts as the ready ack.
    assert_eq!(snapshot["lastResult"]["action"], serde_json::json!("ready"));
    assert_eq!(snapshot["lastResult"]["sequence"], serde_json::json!(0));
}

#[test]
fn subprocess_budget_per_snapshot() {
    let fixture = Fixture::new("budget");
    fixture.reset_log();
    let _ = capture(&fixture, RUST.as_ref(), &["--once"]);
    let calls = fixture.log_lines();
    assert_eq!(
        calls.len(),
        12,
        "expected exactly 12 probe subprocesses, got: {calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("niri msg --json workspaces"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("nmcli"))
            .count(),
        3
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("wpctl"))
            .count(),
        2
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("busctl"))
            .count(),
        3
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("brightnessctl"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("powerprofilesctl"))
            .count(),
        1
    );
}
