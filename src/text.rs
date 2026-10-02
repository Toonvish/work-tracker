//! Small text helpers shared by matching and the UI.

/// Returns `s` cut to at most `max` bytes, never splitting a character.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// Replaces every control character (`\n`, `\r`, `\t`, ESC, ...) with a space.
pub fn sanitize_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ascii_under_at_over() {
        assert_eq!(truncate_bytes("abc", 5), "abc");
        assert_eq!(truncate_bytes("abc", 3), "abc");
        assert_eq!(truncate_bytes("abcdef", 3), "abc");
        assert_eq!(truncate_bytes("abc", 0), "");
    }

    #[test]
    fn truncate_multibyte_straddling_limit() {
        assert_eq!(truncate_bytes("a€", 2), "a");
        assert_eq!(truncate_bytes("a€", 4), "a€");
        assert_eq!(truncate_bytes("€", 1), "");
    }

    #[test]
    fn truncate_emoji_at_cap() {
        let mut s = "a".repeat(65_534);
        s.push('😀'); // bytes 65_534..65_538
        s.push_str("tail");
        let t = truncate_bytes(&s, 65_536);
        assert!(t.len() <= 65_536);
        assert!(s.is_char_boundary(t.len()));
        assert!(s.starts_with(t));
        assert_eq!(t.len(), 65_534);
    }

    #[test]
    fn sanitize_replaces_control_chars() {
        assert_eq!(sanitize_line("a\nb\tc\rd\x1be"), "a b c d e");
        assert_eq!(sanitize_line("plain ü"), "plain ü");
    }
}
