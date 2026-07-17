//! Alert-body text pipeline, ported from Codex's tui text_formatting: turn
//! raw agent output (markdown, multi-line, arbitrary length) into a compact
//! single-line caption. Pure.

use unicode_segmentation::UnicodeSegmentation;

/// Default cap for alert bodies, in graphemes (Codex's value).
pub const BODY_MAX: usize = 200;

/// Strip the visible markdown scaffolding that reads as noise in a one-line
/// caption: emphasis/code markers, heading hashes, list bullets, blockquote
/// markers, and link syntax (keeping the link text). Not a full parser —
/// just the common inline forms.
pub fn strip_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for raw_line in s.lines() {
        let mut line = raw_line.trim_start();
        // Leading block markers.
        line = line.trim_start_matches('#').trim_start();
        line = line.trim_start_matches('>').trim_start();
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            line = rest;
        }
        // Inline emphasis/code markers, dropped wholesale.
        let cleaned: String = line.chars().filter(|c| !matches!(c, '*' | '_' | '`')).collect();
        let cleaned = unlink(&cleaned);
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&cleaned);
    }
    out
}

/// Replace `[text](url)` with `text`.
fn unlink(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let (before, after) = rest.split_at(open);
        out.push_str(before);
        if let Some(close) = after.find("](") {
            let text = &after[1..close];
            if let Some(end) = after[close..].find(')') {
                out.push_str(text);
                rest = &after[close + end + 1..];
                continue;
            }
        }
        out.push('[');
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

/// Collapse every run of whitespace (including newlines/tabs) to a single
/// space and trim the ends.
pub fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Grapheme-aware truncation with an ellipsis, matching Codex's rule: when
/// the input exceeds `max` graphemes, keep `max - 1` and append `…`; when
/// `max < 1` return empty. Uses `…` (one grapheme) as the marker.
pub fn truncate_graphemes(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.graphemes(true).count();
    if count <= max {
        return s.to_string();
    }
    let kept: String = s.graphemes(true).take(max - 1).collect();
    format!("{kept}…")
}

/// Full pipeline at the default cap. `None` when nothing survives cleaning.
pub fn body(s: &str) -> Option<String> {
    body_capped(s, BODY_MAX)
}

/// Full pipeline with a caller-chosen grapheme cap (e.g. 30 for the inline
/// approval templates).
pub fn body_capped(s: &str, max: usize) -> Option<String> {
    let cleaned = collapse_whitespace(&strip_markdown(s));
    if cleaned.is_empty() {
        return None;
    }
    Some(truncate_graphemes(&cleaned, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_common_markdown() {
        assert_eq!(strip_markdown("## Heading"), "Heading");
        assert_eq!(strip_markdown("- a bullet"), "a bullet");
        assert_eq!(strip_markdown("> quoted"), "quoted");
        assert_eq!(strip_markdown("run `cargo test` now"), "run cargo test now");
        assert_eq!(strip_markdown("**bold** and _italic_"), "bold and italic");
        assert_eq!(strip_markdown("see [the docs](http://x)"), "see the docs");
        // A malformed link is left readable, not mangled away.
        assert_eq!(strip_markdown("[unterminated"), "[unterminated");
    }

    #[test]
    fn collapses_all_whitespace() {
        assert_eq!(collapse_whitespace("  a\t\tb\n\nc  "), "a b c");
        assert_eq!(collapse_whitespace("\n\n"), "");
    }

    #[test]
    fn truncates_by_grapheme_with_ellipsis() {
        assert_eq!(truncate_graphemes("hello", 10), "hello");
        assert_eq!(truncate_graphemes("hello", 5), "hello");
        assert_eq!(truncate_graphemes("hello world", 5), "hell…");
        assert_eq!(truncate_graphemes("x", 0), "");
    }

    #[test]
    fn truncation_never_splits_a_grapheme_cluster() {
        // ZWJ family emoji is one grapheme; a naive char/byte cut would
        // shatter it.
        let fam = "👨‍👩‍👧";
        assert_eq!(fam.graphemes(true).count(), 1);
        let s = format!("{fam}{fam}{fam}");
        let out = truncate_graphemes(&s, 2);
        assert_eq!(out, format!("{fam}…"));
        assert!(out.graphemes(true).count() <= 2);
        // Flag pair (regional indicators) stays intact.
        assert_eq!(truncate_graphemes("🇺🇸🇬🇧", 2), "🇺🇸🇬🇧");
    }

    #[test]
    fn body_pipeline_caps_and_empties() {
        let long = "word ".repeat(100);
        let out = body(&long).unwrap();
        assert!(out.graphemes(true).count() <= BODY_MAX);
        assert!(body("   \n\t  ").is_none());
        assert_eq!(
            body("## Ready\n\nAll **tests** pass.").as_deref(),
            Some("Ready All tests pass.")
        );
    }
}
