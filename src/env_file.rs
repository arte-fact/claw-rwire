//! Minimal `.env` loader (no dependency). Reads `KEY=VALUE` lines from `.env` in the
//! current directory and sets any key not already present in the real environment —
//! so a real env var always wins over the file.

use std::path::Path;

/// Load `.env` from the current directory if it exists. Silently does nothing when
/// the file is absent or a line is malformed (best-effort dev convenience).
pub fn load() {
    load_from(Path::new(".env"));
}

fn load_from(path: &Path) {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    for (key, value) in parse(&contents) {
        if std::env::var_os(key).is_none() {
            // SAFETY note: called once at startup, before any threads read the env.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

/// Parse `.env` text into `(key, value)` pairs. Skips blanks and `#` comments,
/// trims whitespace, drops a leading `export `, and strips one layer of matching
/// single or double quotes around the value.
fn parse(contents: &str) -> Vec<(&str, &str)> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            Some((key, unquote(value.trim())))
        })
        .collect()
}

fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        if (first == b'"' || first == b'\'') && bytes[bytes.len() - 1] == first {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pairs_skipping_comments_and_blanks() {
        let text = "# comment\n\nCLAW_MODEL=gpt-4o-mini\n  export CLAW_ENDPOINT = http://x/v1 \n";
        assert_eq!(
            parse(text),
            vec![
                ("CLAW_MODEL", "gpt-4o-mini"),
                ("CLAW_ENDPOINT", "http://x/v1"),
            ]
        );
    }

    #[test]
    fn strips_matching_quotes_only() {
        assert_eq!(unquote("\"sk-abc\""), "sk-abc");
        assert_eq!(unquote("'sk-abc'"), "sk-abc");
        assert_eq!(unquote("sk-abc"), "sk-abc");
        assert_eq!(unquote("\"mismatched'"), "\"mismatched'");
    }

    #[test]
    fn ignores_lines_without_a_key() {
        assert_eq!(parse("=value\nnoequals\n"), Vec::<(&str, &str)>::new());
    }
}
