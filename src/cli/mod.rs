//! One module per subcommand.

/// Make untrusted text safe to print to a terminal.
///
/// Pack metadata and config keys come from files beckon did not write — a
/// cloned repository's `.beckon.toml`, a shared pack. Echoing them raw lets
/// them clear the screen or retitle the window with escape sequences. Printable
/// characters survive; everything else becomes an escape you can see.
pub fn safe(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c == '\t' || (!c.is_control() && c != '\u{7f}') {
                vec![c]
            } else {
                format!("\\u{{{:x}}}", c as u32).chars().collect()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::safe;

    #[test]
    fn escapes_cannot_reach_the_terminal() {
        assert_eq!(safe("\u{1b}[2J"), "\\u{1b}[2J");
        assert_eq!(safe("a\u{7}b"), "a\\u{7}b");
        assert_eq!(
            safe("title\u{1b}]0;OWNED\u{7}"),
            "title\\u{1b}]0;OWNED\\u{7}"
        );
        assert_eq!(safe("\u{7f}"), "\\u{7f}");
    }

    #[test]
    fn ordinary_text_including_unicode_is_untouched() {
        assert_eq!(safe("Aurora — calm"), "Aurora — calm");
        assert_eq!(safe("日本語 😀"), "日本語 😀");
        assert_eq!(safe("tab\there"), "tab\there");
    }
}

pub mod config_cmd;
pub mod doctor;
pub mod hook;
pub mod install;
pub mod mute;
pub mod packs;
pub mod play;
pub mod test_cmd;
