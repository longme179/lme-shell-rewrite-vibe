//! Python-behaviour-compatible helpers.
//!
//! Every function here exists to reproduce a specific Python semantic that
//! is observable through the JSON protocol or on-disk formats: `round()`
//! half-to-even, `or`-defaulting on JSON values, `casefold`-flavoured
//! sorting, `shutil.which`, and POSIX `shlex.split`.

/// Python `round(x)` (banker's rounding) as an integer.
pub fn py_round_int(x: f64) -> i64 {
    let scaled = x;
    let rounded = round_half_even(scaled);
    rounded as i64
}

/// Python `round(x, ndigits)` (banker's rounding) as a float.
pub fn py_round_nd(x: f64, ndigits: i32) -> f64 {
    let factor = 10f64.powi(ndigits);
    round_half_even(x * factor) / factor
}

fn round_half_even(scaled: f64) -> f64 {
    if !scaled.is_finite() {
        return scaled;
    }
    let floor = scaled.floor();
    let frac = scaled - floor;
    if frac == 0.5 {
        // Tie: pick the even neighbour.
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        scaled.round()
    }
}

/// Python `item.get(key) or default` for numbers: `0`, `0.0` and `None`
/// fall back to `default`; any other number is used as f64.
pub fn number_or(value: Option<&serde_json::Value>, default: f64) -> f64 {
    match value {
        Some(serde_json::Value::Number(n)) => {
            let as_f64 = n.as_f64().unwrap_or(default);
            if as_f64 == 0.0 {
                default
            } else {
                as_f64
            }
        }
        _ => default,
    }
}

/// `str(item.get(key) or "")` — null and missing keys become "".
pub fn string_or(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                String::new()
            } else {
                s.clone()
            }
        }
        _ => String::new(),
    }
}

/// ASCII-plus casefold used for app-name sorting. Python `str.casefold`
/// applies full Unicode case folding (e.g. `ß` → `ss`); Rust's
/// `to_lowercase` covers Latin-1 additions except a handful of digraph
/// expansions that do not appear in `.desktop` names in practice.
pub fn casefold(s: &str) -> String {
    let mut out = s.to_lowercase();
    // The two common digraph expansions found in European app names.
    if out.contains('ß') {
        out = out.replace('ß', "ss");
    }
    if out.contains('ﬀ') {
        out = out.replace('ﬀ', "ff");
    }
    if out.contains('ﬁ') {
        out = out.replace('ﬁ', "fi");
    }
    out
}

/// `shutil.which(name)`: search PATH for an executable file.
pub fn which(name: &str) -> Option<std::path::PathBuf> {
    if name.contains('/') {
        let path = std::path::Path::new(name);
        return is_executable(path).then(|| path.to_path_buf());
    }
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Real user id without a libc dependency (matches Python `os.getuid()`).
pub fn current_uid() -> u32 {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(real) = rest.split_whitespace().next() {
                    if let Ok(uid) = real.parse() {
                        return uid;
                    }
                }
            }
        }
    }
    0
}

/// POSIX `shlex.split(s)` (as used with `posix=True`, comments inactive):
/// whitespace splitting, `'…'` literal, `"…"` with `\` escaping `"` and `\`,
/// `\x` escaping anywhere else. Unmatched quotes are an error.
pub fn shlex_split(input: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut chars = input.chars().peekable();
    loop {
        // Skip inter-word whitespace.
        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            return Ok(words);
        }
        let mut word = String::new();
        let mut quoted = false; // inside '…'
        let mut escaped_quote = false; // inside "…"
        let mut has_content = false;
        loop {
            match chars.next() {
                None => {
                    if quoted || escaped_quote {
                        return Err("No closing quotation".to_string());
                    }
                    break;
                }
                Some(c) if c.is_whitespace() && !quoted && !escaped_quote => break,
                Some('\'') if !escaped_quote => {
                    quoted = !quoted;
                    has_content = true;
                }
                Some('"') if !quoted => {
                    escaped_quote = !escaped_quote;
                    has_content = true;
                }
                Some('\\') if !quoted => {
                    has_content = true;
                    match chars.next() {
                        None => {
                            if escaped_quote {
                                return Err("No escaped character".to_string());
                            }
                            // Trailing backslash outside quotes is literal.
                            word.push('\\');
                            break;
                        }
                        Some(next) if escaped_quote && next != '"' && next != '\\' => {
                            word.push('\\');
                            word.push(next);
                        }
                        Some(next) => word.push(next),
                    }
                }
                Some(c) => {
                    has_content = true;
                    word.push(c);
                }
            }
        }
        if has_content || !word.is_empty() {
            words.push(word);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_half_even_matches_python() {
        assert_eq!(py_round_int(86.5), 86);
        assert_eq!(py_round_int(87.5), 88);
        assert_eq!(py_round_int(-0.5), 0);
        assert_eq!(py_round_int(45.00000000000001), 45);
        assert_eq!(py_round_nd(47.25, 1), 47.2);
        assert_eq!(py_round_nd(47.35, 1), 47.4);
        assert_eq!(py_round_nd(5.472, 2), 5.47);
        assert_eq!(py_round_nd(100.0 * 0.44666, 1), 44.7);
    }

    #[test]
    fn shlex_matches_python() {
        assert_eq!(
            shlex_split("gio launch '/my path/app.desktop'").unwrap(),
            vec!["gio", "launch", "/my path/app.desktop"]
        );
        assert_eq!(
            shlex_split(r#"env VAR="a b" cmd\ arg"#).unwrap(),
            vec!["env", "VAR=a b", "cmd arg"]
        );
        assert_eq!(
            shlex_split(r#"printf "%s\n" x"#).unwrap(),
            vec!["printf", "%s\\n", "x"] // \n stays escaped inside dquotes
        );
        assert!(shlex_split("open 'quote").is_err());
        assert_eq!(shlex_split("a '' b").unwrap(), vec!["a", "", "b"]);
    }
}
