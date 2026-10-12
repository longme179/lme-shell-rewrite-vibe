// SPDX-License-Identifier: GPL-3.0-or-later
//! Launcher data: the multi-mode result model (REBUILD_SPEC §6.1), the
//! fuzzy ranking ported verbatim from `qml/services/LauncherSearch.js`,
//! emoji search over `assets/emoji.json`, and a local arithmetic evaluator
//! for the `=` calculator mode (upstream calls a backend action; evaluating
//! locally is strictly better — no IPC round trip — and testable headless;
//! logged in docs/DECISIONS.md D13).

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum ResultKind {
    App { id: String },
    Copy { value: String },
    Window { window_id: i64 },
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LauncherResult {
    pub kind: ResultKind,
    pub name: String,
    pub detail: String,
    pub icon: Option<String>,
    pub favorite: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Apps,
    Calculator,
    Emoji,
    Windows,
}

/// Mode detection, ported from Launcher.qml line 22:
/// `:` emoji · `@` windows · `=` or bare math (digits/space/().+*/%^- that
/// contains an operator) calculator · else apps.
pub fn detect_mode(query: &str) -> Mode {
    let q = query.trim();
    if query.starts_with(':') {
        return Mode::Emoji;
    }
    if query.starts_with('@') {
        return Mode::Windows;
    }
    if query.starts_with('=') {
        return Mode::Calculator;
    }
    let bare_math = !q.is_empty()
        && q.chars()
            .all(|c| c.is_ascii_digit() || c.is_whitespace() || "().+*/%^-".contains(c))
        && q.chars().any(|c| "+*/%^-".contains(c));
    if bare_math {
        Mode::Calculator
    } else {
        Mode::Apps
    }
}

// ---- fuzzy ranking (LauncherSearch.js, line by line) ----

/// Levenshtein distance with the legacy early-exit (length diff > 2 → 3).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 2 {
        return 3;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut row = vec![i];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            row.push(
                (row[j - 1] + 1)
                    .min(previous[j] + 1)
                    .min(previous[j - 1] + cost),
            );
        }
        previous = row;
    }
    previous[b.len()]
}

/// Single-term score against (name, other-fields).
fn score(term: &str, name: &str, other: &str) -> i32 {
    if name == term {
        return 100;
    }
    if name.starts_with(term) {
        return 85;
    }
    if name.contains(term) {
        return 70;
    }
    if other.contains(term) {
        return 55;
    }
    // subsequence with gap penalty
    let mut cursor = 0usize;
    for c in name.chars() {
        if term.chars().nth(cursor) == Some(c) {
            cursor += 1;
        }
        if cursor >= term.chars().count() && cursor > 0 {
            break;
        }
    }
    if cursor == term.chars().count() && cursor > 0 {
        return 40 - (name.chars().count().saturating_sub(term.chars().count())).min(20) as i32;
    }
    // per-word Levenshtein (≤ min(2, len/3))
    let term_len = term.chars().count();
    if (3..=40).contains(&term_len)
        && name
            .split_whitespace()
            .any(|word| distance(term, word) <= term_len / 3.min(term_len))
    {
        return 20;
    }
    -1
}

/// Rank apps for a query. `apps` are backend-scan Value objects
/// ({id,name,icon,genericName,comment,keywords[]}).
pub fn rank_apps(
    query: &str,
    apps: &[Value],
    favorites: &[String],
    only_favorites: bool,
) -> Vec<Value> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .take(8)
        .map(|t| t.to_string())
        .collect();
    let mut ranked: Vec<(Value, i32, bool, String)> = apps
        .iter()
        .filter(|app| {
            !only_favorites
                || favorites.contains(
                    &app.get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                )
        })
        .filter_map(|app| {
            let name = app
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let mut parts: Vec<String> = Vec::new();
            parts.push(
                app.get("genericName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
            parts.push(
                app.get("comment")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
            if let Some(keywords) = app.get("keywords").and_then(Value::as_array) {
                for kw in keywords {
                    if let Some(kw) = kw.as_str() {
                        parts.push(kw.to_string());
                    }
                }
            }
            let other = parts.join(" ").to_lowercase();
            let scores: Vec<i32> = terms.iter().map(|t| score(t, &name, &other)).collect();
            if scores.iter().any(|s| *s < 0) {
                return None;
            }
            let total: i32 = scores.iter().sum();
            let id = app
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let fav = favorites.contains(&id);
            Some((app.clone(), total, fav, name))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.3.cmp(&b.3)));
    ranked.into_iter().map(|(app, _, _, _)| app).collect()
}

// ---- emoji ----

/// Arc-backed so the daemon can clone it into per-refresh borrows cheaply.
#[derive(Clone)]
pub struct EmojiIndex {
    entries: std::sync::Arc<Vec<(String, String, String)>>, // (emoji, name, keywords)
}

impl EmojiIndex {
    pub fn load(path: &std::path::Path) -> EmojiIndex {
        let empty = EmojiIndex {
            entries: std::sync::Arc::new(Vec::new()),
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return empty;
        };
        let Ok(list) = serde_json::from_str::<Vec<Value>>(&text) else {
            return empty;
        };
        let entries: Vec<(String, String, String)> = list
            .into_iter()
            .filter_map(|e| {
                Some((
                    e.get("emoji")?.as_str()?.to_string(),
                    e.get("name")?.as_str()?.to_string(),
                    e.get("keywords")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ))
            })
            .collect();
        EmojiIndex {
            entries: std::sync::Arc::new(entries),
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// (name + " " + keywords).includes(text), first 100 — legacy order.
    pub fn search(&self, text: &str, out: &mut Vec<(String, String)>) {
        out.clear();
        if text.is_empty() {
            return;
        }
        for (emoji, name, keywords) in self.entries.iter() {
            if format!("{name} {keywords}").contains(text) {
                out.push((emoji.clone(), name.clone()));
                if out.len() >= 100 {
                    return;
                }
            }
        }
    }
}

// ---- calculator ----

/// Recursive-descent evaluator for + - * / % ^ parentheses and unary minus.
/// Returns Err on anything else (matches the backend's error surface).
pub fn calculate(expression: &str) -> Result<f64, String> {
    let tokens: Vec<char> = expression.chars().filter(|c| !c.is_whitespace()).collect();
    let mut parser = Parser { tokens, pos: 0 };
    let value = parser.expr()?;
    if parser.pos != parser.tokens.len() {
        return Err("Invalid expression".into());
    }
    if value.is_nan() || value.is_infinite() {
        return Err("Invalid expression".into());
    }
    Ok(value)
}

struct Parser {
    tokens: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.tokens.get(self.pos).copied()
    }
    fn expr(&mut self) -> Result<f64, String> {
        let mut left = self.term()?;
        while let Some(op) = self.peek() {
            if op == '+' || op == '-' {
                self.pos += 1;
                let right = self.term()?;
                left = if op == '+' {
                    left + right
                } else {
                    left - right
                };
            } else {
                break;
            }
        }
        Ok(left)
    }
    fn term(&mut self) -> Result<f64, String> {
        let mut left = self.power()?;
        while let Some(op) = self.peek() {
            if op == '*' || op == '/' || op == '%' {
                self.pos += 1;
                let right = self.power()?;
                left = match op {
                    '*' => left * right,
                    '/' => {
                        if right == 0.0 {
                            return Err("Division by zero".into());
                        }
                        left / right
                    }
                    _ => {
                        if right == 0.0 {
                            return Err("Division by zero".into());
                        }
                        left % right
                    }
                };
            } else {
                break;
            }
        }
        Ok(left)
    }
    fn power(&mut self) -> Result<f64, String> {
        let base = self.unary()?;
        if self.peek() == Some('^') {
            self.pos += 1;
            let exp = self.power()?;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }
    fn unary(&mut self) -> Result<f64, String> {
        if self.peek() == Some('-') {
            self.pos += 1;
            return Ok(-self.unary()?);
        }
        self.primary()
    }
    fn primary(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                let value = self.expr()?;
                if self.peek() != Some(')') {
                    return Err("Invalid expression".into());
                }
                self.pos += 1;
                Ok(value)
            }
            Some(c) if c.is_ascii_digit() || c == '.' => self.number(),
            _ => Err("Invalid expression".into()),
        }
    }
    fn number(&mut self) -> Result<f64, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text: String = self.tokens[start..self.pos].iter().collect();
        text.parse::<f64>()
            .map_err(|_| "Invalid expression".to_string())
    }
}

/// Format a calculation result like the Python backend (repr of float):
/// integers without a trailing .0.
pub fn format_result(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 && value.abs() < 1e15 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, generic: &str, comment: &str, keywords: &[&str]) -> Value {
        serde_json::json!({
            "id": id, "name": name, "icon": "", "genericName": generic,
            "comment": comment, "keywords": keywords
        })
    }

    #[test]
    fn mode_detection_matches_qml() {
        assert_eq!(detect_mode(":"), Mode::Emoji);
        assert_eq!(detect_mode("@fire"), Mode::Windows);
        assert_eq!(detect_mode("=2+2"), Mode::Calculator);
        assert_eq!(detect_mode("2+2"), Mode::Calculator);
        assert_eq!(detect_mode("2 * (3+4)"), Mode::Calculator);
        assert_eq!(detect_mode("firefox"), Mode::Apps);
        assert_eq!(detect_mode("2"), Mode::Apps);
        assert_eq!(detect_mode(""), Mode::Apps);
    }

    #[test]
    fn ranking_exact_prefix_subsequence() {
        let apps = vec![
            app(
                "f",
                "Firefox",
                "browser",
                "Web browser",
                &["web", "internet"],
            ),
            app("fi", "Firewall Config", "", "", &[]),
            app("t", "Terminal", "console", "A terminal", &[]),
        ];
        let ranked = rank_apps("firefox", &apps, &[], false);
        assert_eq!(ranked[0].get("id").and_then(Value::as_str), Some("f"));
        let subseq = rank_apps("ffx", &apps, &[], false);
        assert_eq!(subseq.len(), 1, "subsequence f-f-x matches Firefox only");
        assert_eq!(
            subseq[0].get("id").and_then(Value::as_str),
            Some("f"),
            "the subsequence hit is Firefox"
        );
        let multi = rank_apps("fire", &apps, &[], false);
        assert_eq!(multi.len(), 2);
    }

    #[test]
    fn ranking_favorites_first_on_tie_and_filter() {
        let apps = vec![
            app("a", "Alpha", "", "", &[]),
            app("b", "Beta", "", "", &[]),
        ];
        let ranked = rank_apps("", &apps, &["b".to_string()], false);
        // empty query: every app matches with score 0 (no terms); favorites first
        assert_eq!(ranked[0].get("id").and_then(Value::as_str), Some("b"));
        let only_favs = rank_apps("alp", &apps, &["b".to_string()], true);
        assert!(only_favs.is_empty(), "Alpha is not a favorite");
        let fav_first = rank_apps("alp", &apps, &["a".to_string()], false);
        assert_eq!(fav_first[0].get("id").and_then(Value::as_str), Some("a"));
    }

    #[test]
    fn calculator_matches_expected_results() {
        assert_eq!(calculate("2+2").unwrap(), 4.0);
        assert_eq!(calculate("2 * (3+4)").unwrap(), 14.0);
        assert_eq!(calculate("10/4").unwrap(), 2.5);
        assert_eq!(calculate("2^10").unwrap(), 1024.0);
        assert_eq!(calculate("-3+5").unwrap(), 2.0);
        assert_eq!(calculate("10 % 3").unwrap(), 1.0);
        assert!(calculate("2+").is_err());
        assert!(calculate("foo").is_err());
        assert!(calculate("1/0").is_err());
        assert_eq!(format_result(4.0), "4");
        assert_eq!(format_result(2.5), "2.5");
    }

    #[test]
    fn emoji_search_finds_and_caps() {
        let emoji_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/emoji.json");
        let idx = EmojiIndex::load(&emoji_path);
        assert!(idx.len() > 100, "emoji index loaded");
        let mut out = Vec::new();
        idx.search("rainbow", &mut out);
        assert!(!out.is_empty());
        assert_eq!(out[0].0, "🌈");
        let mut capped = Vec::new();
        idx.search("a", &mut capped);
        assert!(capped.len() <= 100);
    }
}
