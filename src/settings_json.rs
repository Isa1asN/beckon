//! Editing an agent's `settings.json` without disturbing it.
//!
//! This file can make the agent run arbitrary commands, so beckon's edits have
//! to be conservative and exactly reversible:
//!
//! - key order and untouched content are preserved (serde_json `preserve_order`);
//! - beckon's own entries are recognised by their command, so removal never
//!   guesses and never needs a marker field the agent's schema might reject;
//! - `merge` is idempotent, and `unmerge` removes exactly what `merge` added,
//!   leaving every other entry — including hook shapes beckon does not itself
//!   write — untouched.
//!
//! What is *not* promised: byte-for-byte identity for a file that was not
//! already canonical JSON. This goes through `serde_json`, so CRLF becomes LF,
//! indentation is normalised, and duplicate keys collapse. Callers refuse the
//! shapes that cannot survive at all; formatting drift is accepted, and the
//! backup holds the original.

use serde_json::{json, Map, Value};

/// One hook binding: an event name and the command to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub event: &'static str,
    pub command: String,
    pub timeout: u64,
}

/// Everything an adapter wants written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    pub bindings: Vec<Binding>,
}

/// A shape in the settings file that beckon does not understand.
///
/// The response to one of these is always to stop, never to normalise. This is
/// the file that decides what the agent may execute; guessing at an unfamiliar
/// shape is how you delete someone's work.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MergeError {
    #[error("the top level is not a JSON object")]
    RootNotObject,

    #[error("`hooks` is not a JSON object")]
    HooksNotObject,

    #[error("`hooks.{0}` is not a JSON array")]
    EventNotArray(String),
}

/// Split a command into its program and the rest, honouring a quoted path.
///
/// Splitting on whitespace first is wrong: `init` quotes a path containing
/// spaces, and a naive split then reads the program as `"/opt/my`. beckon stops
/// recognising its own entries, `uninstall` becomes a no-op, and every `init`
/// appends another copy.
fn split_program(command: &str) -> Option<(&str, &str)> {
    let trimmed = command.trim_start();
    match trimmed.strip_prefix('"') {
        Some(rest) => {
            let end = rest.find('"')?;
            Some((&rest[..end], &rest[end + 1..]))
        }
        None => {
            let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
            Some((&trimmed[..end], &trimmed[end..]))
        }
    }
}

/// Does this command invoke a program actually named `beckon`?
///
/// The *file name*, not the stem: `beckon.sh` and `beckon.py` are somebody
/// else's scripts, and stem matching would have swept them away.
fn program_is_beckon(command: &str) -> bool {
    let Some((program, _)) = split_program(command) else {
        return false;
    };
    matches!(
        std::path::Path::new(program)
            .file_name()
            .and_then(|n| n.to_str()),
        Some("beckon") | Some("beckon.exe")
    )
}

/// Exactly an invocation beckon itself wrote: `<beckon> hook <agent>`, nothing
/// more.
///
/// Used to decide what may be **removed**, so it is deliberately strict. A user
/// who appended `&& notify-send done` to our line has made it theirs, and
/// `uninstall` must leave it alone rather than discarding their edit.
pub fn is_beckon_command(command: &str) -> bool {
    if !program_is_beckon(command) {
        return false;
    }
    let Some((_, rest)) = split_program(command) else {
        return false;
    };
    let mut tokens = rest.split_whitespace();
    tokens.next() == Some("hook") && tokens.next().is_some() && tokens.next().is_none()
}

/// Is this recognisably a beckon invocation, however it has been edited?
///
/// Used to decide whether to **add** an entry, so it is deliberately loose. The
/// gap between this and [`is_beckon_command`] is exactly the set of commands we
/// will neither remove nor duplicate.
pub fn looks_like_beckon(command: &str) -> bool {
    if !program_is_beckon(command) {
        return false;
    }
    let Some((_, rest)) = split_program(command) else {
        return false;
    };
    rest.split_whitespace().next() == Some("hook")
}

/// Add or refresh beckon's bindings, leaving everything else exactly as it was.
pub fn merge(existing: &Value, plan: &InstallPlan) -> Result<Value, MergeError> {
    let mut root = object_of(existing)?.clone();
    let mut hooks = match root.get("hooks") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err(MergeError::HooksNotObject),
    };

    for binding in &plan.bindings {
        let existing_groups: Vec<Value> = match hooks.get(binding.event) {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(groups)) => groups.clone(),
            Some(_) => return Err(MergeError::EventNotArray(binding.event.to_string())),
        };

        // Drop only entries that are exactly ours, so re-running updates a stale
        // path instead of accumulating copies.
        let mut groups: Vec<Value> = existing_groups
            .into_iter()
            .filter_map(strip_beckon_from_group)
            .collect();

        // A beckon-ish entry someone has edited stays, and stops us adding a
        // second one beside it.
        let already_present = groups.iter().any(group_mentions_beckon);
        if !already_present {
            groups.push(json!({
                "hooks": [{
                    "type": "command",
                    "command": binding.command,
                    "timeout": binding.timeout,
                }]
            }));
        }
        hooks.insert(binding.event.to_string(), Value::Array(groups));
    }

    root.insert("hooks".to_string(), Value::Object(hooks));
    Ok(Value::Object(root))
}

/// Remove every binding beckon wrote, and nothing else.
pub fn unmerge(existing: &Value) -> Result<Value, MergeError> {
    let mut root = object_of(existing)?.clone();
    let Some(hooks) = root.get("hooks").and_then(Value::as_object).cloned() else {
        // No hooks object, or a shape we do not touch: nothing of ours here.
        return Ok(Value::Object(root));
    };

    let mut kept = Map::new();
    let mut removed_anything = false;

    for (event, groups) in hooks {
        let Some(array) = groups.as_array() else {
            // Not an array: not a shape beckon ever wrote, so not ours to prune.
            kept.insert(event, groups);
            continue;
        };

        // An event with nothing of ours in it is left completely alone —
        // including an empty array, which is the user's to keep.
        if !array.iter().any(group_contains_exact_beckon) {
            kept.insert(event, groups.clone());
            continue;
        }

        removed_anything = true;
        let remaining: Vec<Value> = array
            .iter()
            .cloned()
            .filter_map(strip_beckon_from_group)
            .collect();
        if !remaining.is_empty() {
            kept.insert(event, Value::Array(remaining));
        }
        // Otherwise the event held only our entries, so the key goes too.
    }

    if kept.is_empty() && removed_anything {
        root.remove("hooks");
    } else {
        root.insert("hooks".to_string(), Value::Object(kept));
    }
    Ok(Value::Object(root))
}

fn object_of(value: &Value) -> Result<&Map<String, Value>, MergeError> {
    value.as_object().ok_or(MergeError::RootNotObject)
}

/// Commands inside a group, if it has the shape beckon writes.
fn group_commands(group: &Value) -> Option<Vec<&str>> {
    Some(
        group
            .as_object()?
            .get("hooks")?
            .as_array()?
            .iter()
            .filter_map(|entry| entry.get("command")?.as_str())
            .collect(),
    )
}

fn group_contains_exact_beckon(group: &Value) -> bool {
    group_commands(group).is_some_and(|c| c.iter().copied().any(is_beckon_command))
}

fn group_mentions_beckon(group: &Value) -> bool {
    group_commands(group).is_some_and(|c| c.iter().copied().any(looks_like_beckon))
}

/// Strip beckon's own commands from one group.
///
/// `None` means the group held nothing but ours and should go. A group whose
/// shape we do not recognise is returned **untouched** — an earlier version
/// dropped those, which silently deleted legacy flat hooks, groups with an
/// empty `hooks` array, and anyone whose `hooks` was an object rather than an
/// array.
fn strip_beckon_from_group(group: Value) -> Option<Value> {
    let Some(object) = group.as_object() else {
        return Some(group);
    };
    let Some(inner) = object.get("hooks").and_then(Value::as_array) else {
        return Some(group);
    };

    let kept: Vec<Value> = inner
        .iter()
        .filter(|entry| {
            !entry
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(is_beckon_command)
        })
        .cloned()
        .collect();

    if kept.len() == inner.len() {
        return Some(group);
    }
    if kept.is_empty() {
        return None;
    }
    let mut object = object.clone();
    object.insert("hooks".to_string(), Value::Array(kept));
    Some(Value::Object(object))
}

/// Which of `plan`'s bindings are not already present exactly as specified.
pub fn missing_bindings(existing: &Value, plan: &InstallPlan) -> Vec<&'static str> {
    plan.bindings
        .iter()
        .filter(|binding| !has_exact_binding(existing, binding))
        .map(|binding| binding.event)
        .collect()
}

fn has_exact_binding(existing: &Value, binding: &Binding) -> bool {
    existing
        .get("hooks")
        .and_then(|h| h.get(binding.event))
        .and_then(Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|inner| {
                        inner.iter().any(|entry| {
                            entry.get("command").and_then(Value::as_str) == Some(&binding.command)
                        })
                    })
            })
        })
}

/// A unified diff of the pretty-printed JSON, for showing before writing.
///
/// With context, deliberately. Changed lines alone read as unbalanced braces,
/// which is exactly the wrong impression to give someone about to let you edit
/// the file that decides what their agent may execute.
pub fn diff(before: &Value, after: &Value) -> String {
    diff_with_context(before, after, 3)
}

pub fn diff_with_context(before: &Value, after: &Value, context: usize) -> String {
    let before = pretty(before);
    let after = pretty(after);
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();

    // Quadratic in lines; settings files are small, but degrade gracefully
    // rather than allocating a huge table for a pathological one.
    if old.len().max(new.len()) > 2000 {
        return format!("({} lines before, {} lines after)\n", old.len(), new.len());
    }

    let rows = line_diff(&old, &new);
    let changed: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, (marker, _))| *marker != ' ')
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return String::new();
    }

    // Keep any row within `context` of a change.
    let keep: Vec<bool> = (0..rows.len())
        .map(|i| {
            changed.iter().any(|c| {
                let low = c.saturating_sub(context);
                i >= low && i <= c + context
            })
        })
        .collect();

    let mut out = String::new();
    let mut skipping = false;
    for (i, (marker, line)) in rows.iter().enumerate() {
        if keep[i] {
            skipping = false;
            out.push(*marker);
            out.push(' ');
            out.push_str(line);
            out.push('\n');
        } else if !skipping {
            skipping = true;
            out.push_str("  ...\n");
        }
    }
    out
}

/// Hook commands in this file that are not beckon's, as `(event, command)`.
///
/// `init` reports these so the user can see exactly what it is leaving alone.
pub fn foreign_hooks(value: &Value) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let Some(hooks) = value.get("hooks").and_then(Value::as_object) else {
        return found;
    };

    for (event, groups) in hooks {
        for group in groups.as_array().into_iter().flatten() {
            for entry in group
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(command) = entry.get("command").and_then(Value::as_str) {
                    if !is_beckon_command(command) {
                        found.push((event.clone(), command.to_string()));
                    }
                }
            }
        }
    }
    found
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Classic LCS diff. Returns `(marker, line)` with ' ', '-' or '+'.
fn line_diff<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(char, &'a str)> {
    let (n, m) = (old.len(), new.len());
    let mut table = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if old[i] == new[j] {
            out.push((' ', old[i]));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            out.push(('-', old[i]));
            i += 1;
        } else {
            out.push(('+', new[j]));
            j += 1;
        }
    }
    out.extend(old[i..].iter().map(|l| ('-', *l)));
    out.extend(new[j..].iter().map(|l| ('+', *l)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> InstallPlan {
        InstallPlan {
            bindings: vec![
                Binding {
                    event: "Stop",
                    command: "/usr/local/bin/beckon hook claude-code".into(),
                    timeout: 5,
                },
                Binding {
                    event: "Notification",
                    command: "/usr/local/bin/beckon hook claude-code".into(),
                    timeout: 5,
                },
            ],
        }
    }

    fn foreign() -> Value {
        serde_json::from_str(
            r#"{
                "model": "opus",
                "hooks": {
                    "Stop": [
                        {"hooks": [{"type": "command", "command": "/usr/bin/notify-send done"}]}
                    ],
                    "PreToolUse": [
                        {"matcher": "Bash",
                         "hooks": [{"type": "command", "command": "./guard.sh"}]}
                    ]
                }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn our_commands_are_recognized_and_others_are_not() {
        assert!(is_beckon_command("/usr/local/bin/beckon hook claude-code"));
        assert!(is_beckon_command("beckon hook claude-code"));
        assert!(is_beckon_command("/opt/beckon.exe hook claude"));

        assert!(!is_beckon_command("/usr/bin/notify-send done"));
        assert!(!is_beckon_command("beckon-wrapper hook claude-code"));
        assert!(!is_beckon_command("mybeckon hook x"));
        assert!(!is_beckon_command("beckon doctor"));
        assert!(!is_beckon_command("beckon"));
        assert!(!is_beckon_command(""));
        // A neighbour merely mentioning us must not be swept away.
        assert!(!is_beckon_command("echo 'beckon hook claude-code'"));
    }

    #[test]
    fn merging_into_an_empty_file_binds_everything() {
        let merged = merge(&json!({}), &plan()).unwrap();
        assert!(missing_bindings(&merged, &plan()).is_empty());
    }

    #[test]
    fn merging_preserves_unrelated_settings_and_hooks() {
        let merged = merge(&foreign(), &plan()).unwrap();
        assert_eq!(merged["model"], "opus");
        assert_eq!(
            merged["hooks"]["PreToolUse"],
            foreign()["hooks"]["PreToolUse"]
        );
        // Someone else's Stop hook survives alongside ours.
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert!(stop[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("notify-send"));
    }

    #[test]
    fn merging_is_idempotent() {
        let once = merge(&foreign(), &plan()).unwrap();
        let twice = merge(&once, &plan()).unwrap();
        assert_eq!(once, twice, "re-running init must not duplicate entries");
    }

    #[test]
    fn merging_refreshes_a_stale_command_path() {
        let stale = InstallPlan {
            bindings: vec![Binding {
                event: "Stop",
                command: "/old/path/beckon hook claude-code".into(),
                timeout: 5,
            }],
        };
        let merged = merge(&merge(&json!({}), &stale).unwrap(), &plan()).unwrap();
        let commands: Vec<&str> = merged["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["hooks"][0]["command"].as_str().unwrap())
            .collect();
        assert_eq!(commands, vec!["/usr/local/bin/beckon hook claude-code"]);
    }

    #[test]
    fn a_merge_then_unmerge_round_trip_is_byte_identical() {
        // The property that makes `beckon uninstall` trustworthy.
        let original = foreign();
        let restored = unmerge(&merge(&original, &plan()).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&original).unwrap(),
            serde_json::to_string_pretty(&restored).unwrap()
        );
    }

    #[test]
    fn round_trip_holds_for_an_empty_file_too() {
        let restored = unmerge(&merge(&json!({}), &plan()).unwrap()).unwrap();
        assert_eq!(restored, json!({}), "the hooks key should be gone entirely");
    }

    #[test]
    fn key_order_is_preserved() {
        let original: Value =
            serde_json::from_str(r#"{"zebra": 1, "apple": 2, "model": "opus"}"#).unwrap();
        let merged = merge(&original, &plan()).unwrap();
        let keys: Vec<&String> = merged.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["zebra", "apple", "model", "hooks"]);
    }

    #[test]
    fn unmerging_leaves_a_shared_group_intact() {
        // Our command and someone else's inside the same group object.
        let shared: Value = serde_json::from_str(
            r#"{"hooks": {"Stop": [{"hooks": [
                {"type":"command","command":"beckon hook claude-code"},
                {"type":"command","command":"./mine.sh"}
            ]}]}}"#,
        )
        .unwrap();
        let cleaned = unmerge(&shared).unwrap();
        let inner = cleaned["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0]["command"], "./mine.sh");
    }

    #[test]
    fn unmerging_a_file_we_never_touched_changes_nothing() {
        assert_eq!(unmerge(&foreign()).unwrap(), foreign());
    }

    #[test]
    fn missing_bindings_reports_what_is_absent() {
        let partial = merge(
            &json!({}),
            &InstallPlan {
                bindings: vec![plan().bindings[0].clone()],
            },
        )
        .unwrap();
        assert_eq!(missing_bindings(&partial, &plan()), vec!["Notification"]);
    }

    #[test]
    fn the_diff_shows_additions_with_surrounding_context() {
        let before = foreign();
        let after = merge(&before, &plan()).unwrap();
        let text = diff(&before, &after);
        assert!(
            text.lines().any(|l| l.starts_with('+')),
            "no additions:\n{text}"
        );
        assert!(text.contains("beckon hook claude-code"));
        // Context is what makes the diff trustworthy: changed lines alone read
        // as unbalanced braces.
        assert!(
            text.lines().any(|l| l.starts_with("  ")),
            "no context lines:\n{text}"
        );
    }

    #[test]
    fn the_diff_elides_long_unchanged_stretches() {
        let mut big = serde_json::Map::new();
        for i in 0..80 {
            big.insert(format!("key{i}"), json!(i));
        }
        let before = Value::Object(big);
        let after = merge(&before, &plan()).unwrap();
        let text = diff(&before, &after);
        assert!(
            text.contains("..."),
            "long unchanged runs should be elided:\n{text}"
        );
        assert!(text.lines().count() < 130, "diff is too long to read");
    }

    #[test]
    fn foreign_hooks_are_listed_and_ours_are_not() {
        let installed = merge(&foreign(), &plan()).unwrap();
        let others = foreign_hooks(&installed);
        let commands: Vec<&str> = others.iter().map(|(_, c)| c.as_str()).collect();
        assert_eq!(commands.len(), 2, "{others:?}");
        assert!(commands.iter().any(|c| c.contains("notify-send")));
        assert!(commands.iter().any(|c| c.contains("guard.sh")));
        assert!(!commands.iter().any(|c| c.contains("beckon")));
    }

    #[test]
    fn diffing_identical_values_shows_no_changes() {
        let text = diff(&foreign(), &foreign());
        assert!(
            !text
                .lines()
                .any(|l| l.starts_with('+') || l.starts_with('-')),
            "{text}"
        );
    }
}
