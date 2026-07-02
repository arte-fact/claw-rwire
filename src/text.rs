//! Tiny text helpers shared across projections: the "first line, capped" snippet every
//! title/card derivation was hand-rolling with slightly different constants.

/// The first non-empty line of `text`, trimmed and capped at `max_chars` characters.
/// Empty when `text` is blank — callers supply their own fallback.
pub fn first_line_capped(text: &str, max_chars: usize) -> String {
    let line = text.trim().lines().next().unwrap_or("").trim();
    line.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_at_the_limit_and_takes_only_the_first_line() {
        assert_eq!(first_line_capped("hello\nworld", 10), "hello");
        assert_eq!(first_line_capped(&"x".repeat(80), 40).chars().count(), 40);
        assert_eq!(first_line_capped("   \n  ", 10), "");
    }
}
