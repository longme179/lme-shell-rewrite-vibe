//! Bounded, asynchronous Open-Meteo weather with optional IP geolocation
//! (`weather.py` port). A single worker thread runs one job at a time; the
//! snapshot call harvests finished jobs exactly like the Python client.

use crate::prefs::validate_location;
use serde_json::{json, Map, Value};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const UNAVAILABLE: &str = "Weather service is unavailable. Check your connection and retry.";

/// Injectable transport (mirrors the Python `opener` injection).
pub trait Fetch: Send + Sync {
    fn fetch_json(&self, url: &str) -> Result<Value, String>;
}

/// ureq-based production transport: 7 s timeout, 1 MiB response cap,
/// the same User-Agent as the Python client.
pub struct HttpFetch {
    agent: ureq::Agent,
}

impl HttpFetch {
    pub fn new() -> Self {
        let agent = ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(7)))
            .build()
            .new_agent();
        Self { agent }
    }
}

impl Default for HttpFetch {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetch for HttpFetch {
    fn fetch_json(&self, url: &str) -> Result<Value, String> {
        let response = self
            .agent
            .get(url)
            .header("User-Agent", "LME-Shell/1.0 (desktop weather)")
            .header("Accept", "application/json")
            .call()
            .map_err(|_| UNAVAILABLE.to_string())?;
        let mut body: Vec<u8> = Vec::new();
        let reader = std::io::BufReader::new(response.into_body().into_reader());
        let mut limited = reader.take(1024 * 1024 + 1);
        limited
            .read_to_end(&mut body)
            .map_err(|_| UNAVAILABLE.to_string())?;
        if body.len() > 1024 * 1024 {
            return Err("Weather response is too large.".to_string());
        }
        let parsed: Value = serde_json::from_slice(&body).map_err(|_| UNAVAILABLE.to_string())?;
        if !parsed.is_object() {
            return Err("Invalid weather response.".to_string());
        }
        Ok(parsed)
    }
}

use std::io::Read as _;

/// WMO weather code → (label, icon).
fn condition(code: Option<&Value>) -> (&'static str, &'static str) {
    let code = code
        .and_then(Value::as_i64)
        .or_else(|| code.and_then(Value::as_f64).map(|f| f as i64))
        .unwrap_or(-1);
    match code {
        0 => ("Clear sky", "weather-sun"),
        1 => ("Mostly clear", "weather-sun"),
        2 => ("Partly cloudy", "weather-cloud"),
        3 => ("Overcast", "weather-cloud"),
        45 => ("Fog", "weather-cloud"),
        48 => ("Freezing fog", "weather-cloud"),
        51 | 53 | 55 => ("Drizzle", "weather-rain"),
        52 => ("Drizzle", "weather-rain"),
        56 | 57 => ("Freezing drizzle", "weather-rain"),
        61 => ("Light rain", "weather-rain"),
        63 => ("Rain", "weather-rain"),
        65 => ("Heavy rain", "weather-rain"),
        66 | 67 => ("Freezing rain", "weather-rain"),
        71 => ("Light snow", "weather-snow"),
        73 => ("Snow", "weather-snow"),
        75 => ("Heavy snow", "weather-snow"),
        77 => ("Snow grains", "weather-snow"),
        80 | 81 => ("Rain showers", "weather-rain"),
        82 => ("Heavy showers", "weather-rain"),
        85 | 86 => ("Snow showers", "weather-snow"),
        95 => ("Thunderstorm", "weather-storm"),
        96 | 99 => ("Thunderstorm with hail", "weather-storm"),
        _ => ("Unknown conditions", "weather-cloud"),
    }
}

/// `number()` port: finite numbers pass through with their JSON type.
fn number(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => value.cloned().unwrap(),
        Some(Value::Number(n)) => n
            .as_f64()
            .filter(|f| f.is_finite())
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

enum JobKind {
    Forecast(Value, String),
    Detect,
    Search(String),
}

enum JobOutcome {
    Forecast(Result<Map<String, Value>, String>),
    Detect(Result<Value, String>),
    Search(Result<Vec<Value>, String>),
}

enum Message {
    Job {
        kind: JobKind,
        slot: Arc<Mutex<Option<JobOutcome>>>,
    },
}

#[derive(Clone)]
struct Job {
    outcome: Arc<Mutex<Option<JobOutcome>>>,
}

impl Job {
    fn harvest(&self) -> Option<JobOutcome> {
        self.outcome.lock().ok().and_then(|mut slot| slot.take())
    }
}

type MonoClock = fn() -> f64;

fn real_monotonic() -> f64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
}

pub fn real_wall_clock() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

pub struct WeatherClient {
    clock: MonoClock,
    worker: Option<std::thread::JoinHandle<()>>,
    submit: Option<Sender<Message>>,
    forecast_job: Option<Job>,
    geo_job: Option<Job>,
    search_job: Option<Job>,
    forecast_key: Option<(Value, Value, String)>,
    data: Map<String, Value>,
    auto_location: Value,
    forecast_due: f64,
    geo_due: f64,
    error: String,
    geo_error: String,
    search_state: Map<String, Value>,
}

impl WeatherClient {
    pub fn new(fetch: Arc<dyn Fetch>) -> Self {
        Self::with_clocks(fetch, real_monotonic, real_wall_clock)
    }

    pub fn with_clocks(fetch: Arc<dyn Fetch>, clock: MonoClock, wall_clock: fn() -> f64) -> Self {
        let (submit, receiver) = std::sync::mpsc::channel::<Message>();
        let worker_fetch = Arc::clone(&fetch);
        let worker = std::thread::Builder::new()
            .name("lme-weather".to_string())
            .spawn(move || {
                while let Ok(Message::Job { kind, slot }) = receiver.recv() {
                    let outcome = run_job(&worker_fetch, &kind, wall_clock);
                    if let Ok(mut guard) = slot.lock() {
                        *guard = Some(outcome);
                    }
                }
            })
            .expect("weather worker thread");
        Self {
            clock,
            worker: Some(worker),
            submit: Some(submit),
            forecast_job: None,
            geo_job: None,
            search_job: None,
            forecast_key: None,
            data: Map::new(),
            auto_location: json!({}),
            forecast_due: 0.0,
            geo_due: 0.0,
            error: String::new(),
            geo_error: String::new(),
            search_state: search_state_default(),
        }
    }

    fn submit_job(&self, kind: JobKind) -> Option<Job> {
        let slot: Arc<Mutex<Option<JobOutcome>>> = Arc::new(Mutex::new(None));
        if let Some(sender) = &self.submit {
            if sender
                .send(Message::Job {
                    kind,
                    slot: Arc::clone(&slot),
                })
                .is_ok()
            {
                return Some(Job { outcome: slot });
            }
        }
        None
    }

    pub fn detect(&mut self) {
        if self.geo_job.is_none() {
            self.geo_job = self.submit_job(JobKind::Detect);
        }
    }

    pub fn refresh(&mut self, automatic: bool) {
        self.forecast_due = 0.0;
        if automatic {
            self.geo_due = 0.0;
            self.detect();
        }
    }

    pub fn search(&mut self, query: &Value) -> Result<(), String> {
        let Some(text) = query.as_str() else {
            return Err("Enter a city name of 2–100 characters.".to_string());
        };
        let trimmed = text.trim();
        let length = trimmed.chars().count();
        if !(2..=100).contains(&length) {
            return Err("Enter a city name of 2–100 characters.".to_string());
        }
        if self.search_job.is_some() {
            return Err("A location search is already running.".to_string());
        }
        self.search_state = json!({
            "query": trimmed, "loading": true, "results": [], "error": "",
        })
        .as_object()
        .cloned()
        .unwrap();
        self.search_job = self.submit_job(JobKind::Search(trimmed.to_string()));
        Ok(())
    }

    fn harvest_jobs(&mut self, now: f64) {
        if let Some(job) = self.search_job.clone() {
            if let Some(JobOutcome::Search(result)) = job.harvest() {
                let state = &mut self.search_state;
                match result {
                    Ok(results) => {
                        state.insert("results".into(), Value::Array(results));
                        state.insert("error".into(), Value::String(String::new()));
                    }
                    Err(error) => {
                        state.insert("results".into(), Value::Array(Vec::new()));
                        state.insert("error".into(), Value::String(truncate(&error, 180)));
                    }
                }
                state.insert("loading".into(), Value::Bool(false));
                self.search_job = None;
            }
        }
        if let Some(job) = self.geo_job.clone() {
            if let Some(JobOutcome::Detect(result)) = job.harvest() {
                match result {
                    Ok(place) => {
                        self.auto_location = place;
                        self.geo_error = String::new();
                        self.geo_due = now + 3600.0;
                    }
                    Err(error) => {
                        self.geo_error = truncate(&error, 180);
                        self.geo_due = now + 120.0;
                    }
                }
                self.geo_job = None;
            }
        }
        if let Some(job) = self.forecast_job.clone() {
            if let Some(JobOutcome::Forecast(result)) = job.harvest() {
                match result {
                    Ok(data) => {
                        self.data = data;
                        self.error = String::new();
                        self.forecast_due = now + 900.0;
                    }
                    Err(error) => {
                        self.error = truncate(&error, 180);
                        self.forecast_due = now + 120.0;
                    }
                }
                self.forecast_job = None;
            }
        }
    }

    /// `snapshot(preferences, allow_fetch)` port, same key order and masking.
    pub fn snapshot(&mut self, preferences: &Map<String, Value>, allow_fetch: bool) -> Value {
        let now = (self.clock)();
        self.harvest_jobs(now);
        let automatic = preferences
            .get("weatherAutoLocation")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if automatic && allow_fetch && now >= self.geo_due && self.geo_job.is_none() {
            self.detect();
        }
        let place = if automatic {
            self.auto_location.clone()
        } else {
            preferences
                .get("weatherLocation")
                .cloned()
                .unwrap_or_else(|| json!({}))
        };
        let unit = preferences
            .get("weatherUnit")
            .and_then(Value::as_str)
            .unwrap_or("celsius")
            .to_string();
        let key = if place.as_object().is_some_and(|map| !map.is_empty()) {
            Some((
                place.get("latitude").cloned().unwrap_or(Value::Null),
                place.get("longitude").cloned().unwrap_or(Value::Null),
                unit.clone(),
            ))
        } else {
            None
        };
        if key != self.forecast_key && self.forecast_job.is_none() {
            self.forecast_key = key.clone();
            self.data = Map::new();
            self.error = String::new();
            self.forecast_due = 0.0;
        }
        if !place.as_object().is_some_and(|map| map.is_empty())
            && allow_fetch
            && self.forecast_job.is_none()
            && (now >= self.forecast_due || key != self.forecast_key)
            && key == self.forecast_key
        {
            self.forecast_job = self.submit_job(JobKind::Forecast(place.clone(), unit.clone()));
        }
        // Hide results belonging to an old location/unit while its request completes.
        let matches = key == self.forecast_key;
        let mut result = if matches {
            self.data.clone()
        } else {
            Map::new()
        };
        result.insert(
            "available".into(),
            Value::Bool(!self.data.is_empty() && matches),
        );
        result.insert(
            "configured".into(),
            Value::Bool(automatic || !place.as_object().is_some_and(|map| map.is_empty())),
        );
        result.insert("automatic".into(), Value::Bool(automatic));
        result.insert("location".into(), place);
        result.insert(
            "loading".into(),
            Value::Bool(self.geo_job.is_some() || self.forecast_job.is_some()),
        );
        result.insert(
            "stale".into(),
            Value::Bool(!self.error.is_empty() || (automatic && !self.geo_error.is_empty())),
        );
        result.insert(
            "error".into(),
            Value::String(if automatic && !self.geo_error.is_empty() {
                self.geo_error.clone()
            } else {
                self.error.clone()
            }),
        );
        result.insert("search".into(), Value::Object(self.search_state.clone()));
        Value::Object(result)
    }

    pub fn close(&mut self) {
        // Mirror the Python `executor.shutdown(wait=False)`: never block on
        // an in-flight request (e.g. a slow DNS lookup at teardown). The
        // detached thread dies with the process.
        if let Some(sender) = self.submit.take() {
            drop(sender);
        }
        self.worker.take();
    }
}

impl Drop for WeatherClient {
    fn drop(&mut self) {
        self.close();
    }
}

fn search_state_default() -> Map<String, Value> {
    json!({"query": "", "loading": false, "results": [], "error": ""})
        .as_object()
        .cloned()
        .unwrap()
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn run_job(fetch: &Arc<dyn Fetch>, kind: &JobKind, wall_clock: fn() -> f64) -> JobOutcome {
    match kind {
        JobKind::Detect => JobOutcome::Detect(detect_job(fetch)),
        JobKind::Search(query) => JobOutcome::Search(search_job(fetch, query)),
        JobKind::Forecast(place, unit) => {
            JobOutcome::Forecast(forecast_job(fetch, place, unit, wall_clock))
        }
    }
}

fn detect_job(fetch: &Arc<dyn Fetch>) -> Result<Value, String> {
    let url = "https://ipwho.is/?fields=success%2Cmessage%2Ccity%2Ccountry%2Clatitude%2Clongitude";
    let data = fetch.fetch_json(url)?;
    if data.get("success") != Some(&Value::Bool(true)) {
        return Err(
            "Automatic location is unavailable. Retry or choose a city in Settings.".to_string(),
        );
    }
    let name = data
        .get("city")
        .and_then(Value::as_str)
        .filter(|city| !city.is_empty())
        .or_else(|| data.get("country").and_then(Value::as_str))
        .unwrap_or("Current location");
    validate_location(&json!({
        "name": name,
        "country": data.get("country").and_then(Value::as_str).unwrap_or(""),
        "latitude": data.get("latitude").cloned().unwrap_or(Value::Null),
        "longitude": data.get("longitude").cloned().unwrap_or(Value::Null),
    }))
}

fn search_job(fetch: &Arc<dyn Fetch>, query: &str) -> Result<Vec<Value>, String> {
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=6&language=en&format=json",
        urlencode(query)
    );
    let data = fetch.fetch_json(&url)?;
    let mut results = Vec::new();
    if let Some(items) = data.get("results").and_then(Value::as_array) {
        for item in items.iter().take(6) {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
            let admin = item.get("admin1").and_then(Value::as_str).unwrap_or("");
            let display = if !admin.is_empty() && admin != name {
                format!("{name}, {admin}")
            } else {
                name.to_string()
            };
            let candidate = json!({
                "name": display,
                "country": item.get("country").and_then(Value::as_str).unwrap_or(""),
                "latitude": item.get("latitude").cloned().unwrap_or(Value::Null),
                "longitude": item.get("longitude").cloned().unwrap_or(Value::Null),
            });
            if let Ok(place) = validate_location(&candidate) {
                results.push(place);
            }
        }
    }
    Ok(results)
}

fn forecast_job(
    fetch: &Arc<dyn Fetch>,
    place: &Value,
    unit: &str,
    wall_clock: fn() -> f64,
) -> Result<Map<String, Value>, String> {
    let latitude = place
        .get("latitude")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let longitude = place
        .get("longitude")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let wind_unit = if unit == "fahrenheit" { "mph" } else { "kmh" };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&timezone=auto&forecast_days=3&temperature_unit={}&wind_speed_unit={}&current=temperature_2m%2Capparent_temperature%2Crelative_humidity_2m%2Cweather_code%2Cis_day%2Cwind_speed_10m&daily=weather_code%2Ctemperature_2m_max%2Ctemperature_2m_min%2Cprecipitation_probability_max",
        urlencode(&format!("{latitude}")),
        urlencode(&format!("{longitude}")),
        urlencode(unit),
        wind_unit,
    );
    let data = fetch.fetch_json(&url)?;
    let current = data.get("current").cloned().unwrap_or_else(|| json!({}));
    let temperature = number(current.get("temperature_2m"));
    if temperature.is_null() {
        return Err("The weather service returned no valid temperature.".to_string());
    }
    let (condition_text, icon) = condition(current.get("weather_code"));
    let mut forecast: Vec<Value> = Vec::new();
    if let Some(daily) = data.get("daily") {
        let dates = daily
            .get("time")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (index, date) in dates.iter().take(3).enumerate() {
            let day = |key: &str| -> Value {
                daily
                    .get(key)
                    .and_then(Value::as_array)
                    .and_then(|values| values.get(index))
                    .map(|value| number(Some(value)))
                    .unwrap_or(Value::Null)
            };
            forecast.push(json!({
                "date": date, "min": day("temperature_2m_min"),
                "max": day("temperature_2m_max"),
                "rainChance": day("precipitation_probability_max"),
            }));
        }
    }
    let mut result = Map::new();
    result.insert("temperature".into(), temperature);
    result.insert(
        "feelsLike".into(),
        number(current.get("apparent_temperature")),
    );
    result.insert(
        "humidity".into(),
        number(current.get("relative_humidity_2m")),
    );
    result.insert("wind".into(), number(current.get("wind_speed_10m")));
    result.insert(
        "condition".into(),
        Value::String(condition_text.to_string()),
    );
    result.insert("icon".into(), Value::String(icon.to_string()));
    result.insert("daily".into(), Value::Array(forecast));
    result.insert(
        "unit".into(),
        Value::String(if unit == "fahrenheit" { "°F" } else { "°C" }.to_string()),
    );
    result.insert(
        "windUnit".into(),
        Value::String(if unit == "fahrenheit" { "mph" } else { "km/h" }.to_string()),
    );
    result.insert("updatedAt".into(), json!(wall_clock()));
    Ok(result)
}

/// Minimal percent-encoding for query components (urlencode default).
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct ScriptedFetch {
        responses: Mutex<VecDeque<Result<Value, String>>>,
        urls: Mutex<Vec<String>>,
    }

    // SAFETY-free test helper: all state is behind Mutex.
    unsafe impl Send for ScriptedFetch {}
    unsafe impl Sync for ScriptedFetch {}

    impl Fetch for ScriptedFetch {
        fn fetch_json(&self, url: &str) -> Result<Value, String> {
            self.urls.lock().unwrap().push(url.to_string());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(UNAVAILABLE.to_string()))
        }
    }

    fn scripted(responses: Vec<Result<Value, String>>) -> Arc<ScriptedFetch> {
        Arc::new(ScriptedFetch {
            responses: Mutex::new(responses.into()),
            urls: Mutex::new(Vec::new()),
        })
    }

    fn preferences() -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("weatherAutoLocation".into(), Value::Bool(false));
        map.insert(
            "weatherLocation".into(),
            json!({"name": "Hanoi", "country": "VN", "latitude": 21.02, "longitude": 105.8}),
        );
        map.insert("weatherUnit".into(), json!("celsius"));
        map
    }

    fn forecast_body() -> Value {
        json!({
            "current": {"temperature_2m": 21.3, "apparent_temperature": 22.0,
                        "relative_humidity_2m": 70, "weather_code": 2, "is_day": 1,
                        "wind_speed_10m": 10.5},
            "daily": {"time": ["2026-10-11", "2026-10-12", "2026-10-13", "2026-10-14"],
                      "temperature_2m_max": [30, 31, 32, 33],
                      "temperature_2m_min": [22, 21, 22, 23],
                      "precipitation_probability_max": [10, 65, 0, 5]}
        })
    }

    #[test]
    fn snapshot_without_fetch_is_empty_but_configured() {
        let mut client = WeatherClient::new(scripted(Vec::new()));
        let snapshot = client.snapshot(&preferences(), false);
        assert_eq!(snapshot["available"], json!(false));
        assert_eq!(snapshot["configured"], json!(true));
        assert_eq!(snapshot["automatic"], json!(false));
        assert_eq!(snapshot["location"]["name"], json!("Hanoi"));
        assert_eq!(snapshot["error"], json!(""));
        assert_eq!(snapshot["search"]["loading"], json!(false));
    }

    #[test]
    fn forecast_key_masking_across_unit_change() {
        let fetch = scripted(vec![Ok(forecast_body())]);
        let mut client = WeatherClient::new(fetch.clone());
        let mut prefs = preferences();
        // First harvest: no fetch allowed, forecast key becomes manual place.
        let first = client.snapshot(&prefs, false);
        assert_eq!(first["available"], json!(false));
        // Allow fetch: job starts, loading true, old data hidden.
        let second = client.snapshot(&prefs, true);
        assert_eq!(second["loading"], json!(true));
        assert_eq!(second["available"], json!(false));
        // Wait for the worker to finish, then harvest.
        std::thread::sleep(Duration::from_millis(200));
        let third = client.snapshot(&prefs, true);
        assert_eq!(third["available"], json!(true));
        assert_eq!(third["temperature"], json!(21.3));
        assert_eq!(third["daily"].as_array().unwrap().len(), 3);
        assert_eq!(third["daily"][1]["rainChance"], json!(65));
        assert_eq!(third["condition"], json!("Partly cloudy"));
        assert_eq!(third["icon"], json!("weather-cloud"));
        // Unit switch masks the stale data until the new forecast lands.
        prefs.insert("weatherUnit".into(), json!("fahrenheit"));
        let masked = client.snapshot(&prefs, true);
        assert_eq!(masked["available"], json!(false));
        assert!(masked.get("temperature").is_none());
    }

    #[test]
    fn search_validates_query_and_reports_errors() {
        let mut client = WeatherClient::new(scripted(Vec::new()));
        assert!(client.search(&json!("x")).is_err());
        assert!(client.search(&json!(123)).is_err());
        client.search(&json!("  hanoi  ")).unwrap();
        assert_eq!(
            client.search(&json!("hanoi")).unwrap_err(),
            "A location search is already running."
        );
    }

    #[test]
    fn urlencode_matches_python_urllib() {
        assert_eq!(urlencode("Hanoi"), "Hanoi");
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("21.02"), "21.02");
    }
}
