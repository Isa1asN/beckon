//! Making text from untrusted files safe to show.
//!
//! Pack metadata, config keys, pack ids and directory names come from files
//! beckon did not write — a cloned repository's `.beckon.toml`, a shared pack,
//! whatever a repository is called. Echoed raw, they can clear the screen,
//! retitle the window or write the clipboard with escape sequences, and with
//! invisible bidi controls make a line read differently from what it is.

/// Make untrusted text safe to print to a terminal or write to a log.
///
/// Printable characters survive; everything else becomes an escape you can
/// see, so nothing can act on the terminal and nothing can hide.
pub fn safe(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c == '\t' || !(c.is_control() || hidden(c)) {
                vec![c]
            } else {
                format!("\\u{{{:x}}}", c as u32).chars().collect()
            }
        })
        .collect()
}

/// [`safe`] for a path — directory names are chosen by whoever made them,
/// including whoever named the repository you cloned.
pub fn safe_path(path: &std::path::Path) -> String {
    safe(&path.display().to_string())
}

/// Characters that print as nothing but change how the text around them
/// reads: bidi overrides and isolates, zero-width characters, separators that
/// break a line without a newline, the BOM, interlinear annotation and tag
/// characters. `U+202E` turns `gnp.exe` into what looks like `exe.png`.
pub fn hidden(c: char) -> bool {
    matches!(c,
        '\u{ad}'
        | '\u{61c}'
        | '\u{180e}'
        | '\u{200b}'..='\u{200f}'
        | '\u{2028}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}'
        | '\u{feff}'
        | '\u{fff9}'..='\u{fffb}'
        | '\u{e0000}'..='\u{e007f}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_cannot_reach_the_terminal() {
        assert_eq!(safe("\u{1b}[2J"), "\\u{1b}[2J");
        assert_eq!(safe("a\u{7}b"), "a\\u{7}b");
        assert_eq!(
            safe("title\u{1b}]0;OWNED\u{7}"),
            "title\\u{1b}]0;OWNED\\u{7}"
        );
        assert_eq!(safe("\u{7f}"), "\\u{7f}");
        assert_eq!(safe("\u{9b}31m"), "\\u{9b}31m", "C1 CSI");
        assert_eq!(safe("line\nforged"), "line\\u{a}forged");
    }

    #[test]
    fn invisible_formatting_is_made_visible() {
        assert_eq!(safe("rlo\u{202e}gnp.exe"), "rlo\\u{202e}gnp.exe");
        assert_eq!(safe("a\u{200b}b"), "a\\u{200b}b");
        assert_eq!(safe("\u{2066}x\u{2069}"), "\\u{2066}x\\u{2069}");
        assert_eq!(safe("\u{feff}bom"), "\\u{feff}bom");
        assert_eq!(safe("tag\u{e0041}"), "tag\\u{e0041}");
    }

    #[test]
    fn ordinary_text_including_unicode_is_untouched() {
        assert_eq!(safe("Aurora — calm"), "Aurora — calm");
        assert_eq!(safe("日本語 😀 العربية"), "日本語 😀 العربية");
        assert_eq!(safe("tab\there"), "tab\there");
    }
}
