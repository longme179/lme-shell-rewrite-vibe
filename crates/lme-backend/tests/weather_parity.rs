//! M5: weather state-machine parity. The Python client (injected opener and
//! step clock, like its own unit tests) and the Rust client (injected
//! transport, same step clock) must produce identical `weather` JSON across
//! a scripted sequence: auto-detect, forecast, TTL window, failing re-fetch
//! with error backoff, and city search.

mod common;

use common::{python3, python_tree};
use lme_backend::weather::{Fetch, WeatherClient};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const FORECAST_BODY: &str = r#"{
  "current": {"temperature_2m": 21.3, "apparent_temperature": 22.5,
              "relative_humidity_2m": 70, "weather_code": 61, "is_day": 1,
              "wind_speed_10m": 10.5},
  "daily": {"time": ["2026-10-11", "2026-10-12"],
            "temperature_2m_max": [30, 31],
            "temperature_2m_min": [22, 21],
            "precipitation_probability_max": [10, 65]}
}"#;
const GEO_BODY: &str = r#"{"success": true, "city": "Da Nang", "country": "Vietnam", "latitude": 16.05, "longitude": 108.2}"#;
const SEARCH_BODY: &str = r#"{"results": [{"name": "Hanoi", "admin1": "Hanoi", "country": "Vietnam", "latitude": 21.03, "longitude": 105.8},
                                        {"name": "Ha Long", "admin1": "Quang Ninh", "country": "Vietnam", "latitude": 20.9, "longitude": 107.2}]}"#;
const UNAVAILABLE: &str = "Weather service is unavailable. Check your connection and retry.";
/// Snapshot-time sequence; index clamps at the end like the Python driver.
const CLOCK_SEQUENCE: [f64; 7] = [0.0, 1.0, 100.0, 200.0, 1000.0, 1100.0, 1200.0];

struct ScriptedFetch {
    forecast_calls: AtomicUsize,
    urls: Mutex<Vec<String>>,
}

impl Fetch for ScriptedFetch {
    fn fetch_json(&self, url: &str) -> Result<Value, String> {
        self.urls.lock().unwrap().push(url.to_string());
        if url.starts_with("https://ipwho.is/") {
            Ok(serde_json::from_str(GEO_BODY).unwrap())
        } else if url.starts_with("https://api.open-meteo.com/v1/forecast") {
            // The TTL re-fetch (second forecast) fails in both drivers.
            if self.forecast_calls.fetch_add(1, Ordering::SeqCst) >= 1 {
                Err(UNAVAILABLE.to_string())
            } else {
                Ok(serde_json::from_str(FORECAST_BODY).unwrap())
            }
        } else if url.starts_with("https://geocoding-api.open-meteo.com/v1/search") {
            Ok(serde_json::from_str(SEARCH_BODY).unwrap())
        } else {
            Err(UNAVAILABLE.to_string())
        }
    }
}

thread_local! {
    static STEP: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn step_clock() -> f64 {
    STEP.with(|step| {
        let index = step.get().min(CLOCK_SEQUENCE.len() - 1);
        step.set(step.get() + 1);
        CLOCK_SEQUENCE[index]
    })
}

fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(250));
}

#[test]
fn weather_state_machine_matches_python() {
    let Some(python) = python3() else {
        eprintln!("python3 unavailable; skipping live weather parity");
        return;
    };
    let driver = format!(
        r#"
import json, sys, time
from concurrent.futures import ThreadPoolExecutor
sys.path.insert(0, {scripts:?})
import weather

CLOCK = [0.0, 1.0, 100.0, 200.0, 1000.0, 1100.0, 1200.0]
FORECAST_BODY = json.loads({forecast:?})
GEO_BODY = json.loads({geo:?})
SEARCH_BODY = json.loads({search:?})
UNAVAILABLE = "Weather service is unavailable. Check your connection and retry."

class Scripted:
    def __init__(self):
        self.forecast_calls = 0
    def __call__(self, url):
        if url.startswith("https://ipwho.is/"):
            return GEO_BODY
        if url.startswith("https://api.open-meteo.com/v1/forecast"):
            self.forecast_calls += 1
            if self.forecast_calls >= 2:
                raise ValueError(UNAVAILABLE)
            return FORECAST_BODY
        if url.startswith("https://geocoding-api.open-meteo.com/v1/search"):
            return SEARCH_BODY
        raise ValueError(UNAVAILABLE)

step = [0]
def clock():
    index = min(step[0], len(CLOCK) - 1)
    step[0] += 1
    return CLOCK[index]

client = weather.WeatherClient(
    opener=Scripted(),
    executor=ThreadPoolExecutor(max_workers=1),
    clock=clock,
    wall_clock=lambda: 1750000000.0,
)

def settle():
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        jobs = [client.geo_job, client.forecast_job, client.search_job]
        if all(job is None or job.done() for job in jobs):
            return
        time.sleep(0.01)

prefs = {{"weatherAutoLocation": True, "weatherUnit": "celsius"}}
steps = []
steps.append(client.snapshot(prefs, allow_fetch=True))  # t=0 submit
settle()
steps.append(client.snapshot(prefs, allow_fetch=True))  # t=1 harvest
steps.append(client.snapshot(prefs, allow_fetch=True))  # t=100 TTL window
steps.append(client.snapshot(prefs, allow_fetch=True))  # t=200 TTL window
steps.append(client.snapshot(prefs, allow_fetch=True))  # t=1000 re-fetch fails
settle()
steps.append(client.snapshot(prefs, allow_fetch=True))  # t=1100 error retained
client.search("hanoi")
settle()
steps.append(client.snapshot(prefs, allow_fetch=True))  # t=1200 search results
print(json.dumps(steps, ensure_ascii=False))
"#,
        scripts = common::python_tree().join("scripts").display().to_string(),
        forecast = FORECAST_BODY,
        geo = GEO_BODY,
        search = SEARCH_BODY,
    );
    let output = std::process::Command::new(python)
        .arg("-c")
        .arg(driver)
        .env("PYTHONIOENCODING", "utf-8")
        .output()
        .expect("run python weather driver");
    assert!(
        output.status.success(),
        "python weather driver failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let python_steps: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("python driver JSON");
    let python_steps = python_steps.as_array().expect("array of steps");
    assert_eq!(python_steps.len(), 7);

    // ---- Rust side, same script ----
    let fetch = Arc::new(ScriptedFetch {
        forecast_calls: AtomicUsize::new(0),
        urls: Mutex::new(Vec::new()),
    });
    let mut client = WeatherClient::with_clocks(fetch, step_clock, || 1750000000.0);
    let prefs: Map<String, Value> = serde_json::from_value(json!({
        "weatherAutoLocation": true, "weatherUnit": "celsius",
    }))
    .unwrap();

    let mut rust_steps: Vec<Value> = Vec::new();
    rust_steps.push(client.snapshot(&prefs, true)); // t=0
    settle();
    rust_steps.push(client.snapshot(&prefs, true)); // t=1
    rust_steps.push(client.snapshot(&prefs, true)); // t=100
    rust_steps.push(client.snapshot(&prefs, true)); // t=200
    rust_steps.push(client.snapshot(&prefs, true)); // t=1000
    settle();
    rust_steps.push(client.snapshot(&prefs, true)); // t=1100
    client.search(&json!("hanoi")).unwrap();
    settle();
    rust_steps.push(client.snapshot(&prefs, true)); // t=1200

    assert_eq!(rust_steps.len(), 7);
    for (index, (python_step, rust_step)) in python_steps.iter().zip(rust_steps.iter()).enumerate()
    {
        assert_eq!(
            python_step, rust_step,
            "weather step {index} differs\npython: {python_step}\nrust:   {rust_step}"
        );
    }
}
