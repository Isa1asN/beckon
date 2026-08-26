//! Writing to the user's `config.toml`.
//!
//! Uses `toml_edit` rather than parse-and-reserialize, because a config file is
//! something a person wrote: their comments, key order and spacing survive an
//! edit made by `beckon use` or `beckon config set`.

use crate::core::config::KNOWN_KEYS;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Value};

/// Expand a leading `~/`, matching what the loader does.
fn expand_home(raw: &str) -> std::path::PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(dirs) = directories::BaseDirs::new() {
            return dirs.home_dir().join(rest);
        }
    }
    std::path::PathBuf::from(raw)
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("unknown setting `{key}`\n\nValid settings:\n{valid}")]
    UnknownKey { key: String, valid: String },

    #[error("`{value}` is not valid for `{key}` — expected {expected}")]
    BadValue {
        key: String,
        value: String,
        expected: String,
    },

    #[error("{0} could not be read: {1}")]
    Read(String, String),

    #[error("{0} is not valid TOML: {1}\nRefusing to touch it.")]
    Parse(String, String),

    #[error("{0} could not be written: {1}")]
    Write(String, String),
}

/// Every settable key, dotted, for error messages and validation.
pub fn settable_keys() -> Vec<String> {
    let mut keys = Vec::new();
    for (table, fields) in KNOWN_KEYS {
        for field in *fields {
            // These are tables, not values.
            if matches!(
                *field,
                "policy" | "events" | "identity" | "remote" | "sounds"
            ) {
                continue;
            }
            keys.push(if table.is_empty() {
                (*field).to_string()
            } else {
                format!("{table}.{field}")
            });
        }
    }
    for state in crate::core::event::State::ALL {
        keys.push(format!("events.{state}"));
        keys.push(format!("sounds.{state}"));
    }
    keys.sort();
    keys
}

fn is_known(key: &str) -> bool {
    settable_keys().iter().any(|k| k == key)
}

/// Set one dotted key in the user's config, creating the file if needed.
pub fn set(path: &Path, key: &str, raw: &str) -> Result<(), EditError> {
    if !is_known(key) {
        return Err(EditError::UnknownKey {
            key: key.to_string(),
            valid: settable_keys()
                .iter()
                .map(|k| format!("  {k}"))
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }

    // Serialise the read-modify-write. Two `beckon config set` calls racing —
    // a bootstrap script setting several keys at once — silently lost one of
    // them, and a lost `enabled = true` leaves beckon quiet with no sign that
    // the change never landed. Silence is the one failure we must not race into.
    let _guard = crate::core::state::lock_path(path);

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(EditError::Read(path.display().to_string(), e.to_string())),
    };

    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| EditError::Parse(path.display().to_string(), format!("{e}")))?;

    let value = typed_value(key, raw)?;

    match key.split_once('.') {
        Some((table, field)) => {
            // `implicit(false)` so the table is actually emitted when new.
            let entry = doc.entry(table).or_insert_with(|| {
                let mut t = toml_edit::Table::new();
                t.set_implicit(false);
                Item::Table(t)
            });
            let Some(table_ref) = entry.as_table_mut() else {
                return Err(EditError::BadValue {
                    key: key.to_string(),
                    value: raw.to_string(),
                    expected: "a table at that path, but the file has a plain value there"
                        .to_string(),
                });
            };
            table_ref[field] = Item::Value(value);
        }
        None => doc[key] = Item::Value(value),
    }

    write_atomic(path, doc.to_string().as_bytes())
        .map_err(|e| EditError::Write(path.display().to_string(), e.to_string()))
}

/// Interpret `raw` according to what the key expects.
fn typed_value(key: &str, raw: &str) -> Result<Value, EditError> {
    let bad = |expected: &str| EditError::BadValue {
        key: key.to_string(),
        value: raw.to_string(),
        expected: expected.to_string(),
    };

    Ok(match key {
        "enabled" | "identity.per_project" => {
            Value::from(parse_bool(raw).ok_or_else(|| bad("true or false"))?)
        }
        _ if key.starts_with("events.") => {
            Value::from(parse_bool(raw).ok_or_else(|| bad("true or false"))?)
        }
        _ if key.starts_with("sounds.") => {
            // Check it now. A path typo that only shows up as silence three
            // days later is the exact failure mode beckon exists to avoid.
            let expanded = expand_home(raw);
            if let Err(e) = crate::audio::sample::load(&expanded) {
                return Err(EditError::BadValue {
                    key: key.to_string(),
                    value: raw.to_string(),
                    expected: format!("a readable audio file ({e})"),
                });
            }
            Value::from(raw)
        }
        "volume" => {
            let v: f64 = raw.parse().map_err(|_| bad("a number between 0 and 1"))?;
            if !(0.0..=1.0).contains(&v) {
                return Err(bad("a number between 0 and 1"));
            }
            Value::from(v)
        }
        "policy.min_turn_seconds" | "policy.rate_limit_ms" => {
            Value::from(raw.parse::<i64>().map_err(|_| bad("a whole number"))?)
        }
        "policy.quiet_hours" => {
            raw.parse::<crate::core::config::QuietHours>()
                .map_err(|_| bad("a window like 23:00-08:00"))?;
            Value::from(raw)
        }
        "policy.quiet_hours_action" => {
            raw.parse::<crate::core::config::QuietAction>()
                .map_err(|_| bad("`silence` or `volume:0.2`"))?;
            Value::from(raw)
        }
        "policy.always_alert" => {
            let mut array = toml_edit::Array::new();
            for name in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                crate::core::event::State::parse(name)
                    .ok_or_else(|| bad("a comma-separated list of state names"))?;
                array.push(name);
            }
            Value::Array(array)
        }
        "remote.mode" => {
            if !["auto", "off", "always", "both"].contains(&raw) {
                return Err(bad("auto, off, always or both"));
            }
            Value::from(raw)
        }
        "remote.sequences" => {
            let mut array = toml_edit::Array::new();
            for name in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if !["bel", "osc9", "osc777"].contains(&name) {
                    return Err(bad("a comma-separated list of bel, osc9, osc777"));
                }
                array.push(name);
            }
            Value::Array(array)
        }
        // `pack` and anything else textual.
        _ => Value::from(raw),
    })
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("beckon-tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use crate::core::paths::Paths;

    fn home() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::resolve_with(Some(d.path()));
        (d, p)
    }

    fn read(paths: &Paths) -> String {
        std::fs::read_to_string(&paths.config_file).unwrap()
    }

    #[test]
    fn setting_a_key_creates_the_file() {
        let (_d, p) = home();
        set(&p.config_file, "pack", "cipher").unwrap();
        assert_eq!(Config::load(&p, None).pack, "cipher");
    }

    #[test]
    fn setting_a_nested_key_creates_its_table() {
        let (_d, p) = home();
        set(&p.config_file, "policy.min_turn_seconds", "12").unwrap();
        assert_eq!(Config::load(&p, None).policy.min_turn_seconds, 12);
        assert!(read(&p).contains("[policy]"), "{}", read(&p));
    }

    #[test]
    fn comments_and_unrelated_keys_survive_an_edit() {
        // The whole reason for toml_edit: this is a file a person wrote.
        let (_d, p) = home();
        std::fs::create_dir_all(p.config_file.parent().unwrap()).unwrap();
        std::fs::write(
            &p.config_file,
            "# my settings\npack = \"aurora\"  # I like this one\nvolume = 0.4\n",
        )
        .unwrap();

        set(&p.config_file, "volume", "0.8").unwrap();
        let text = read(&p);
        assert!(
            text.contains("# my settings"),
            "lost the header comment:\n{text}"
        );
        assert!(
            text.contains("# I like this one"),
            "lost the inline comment:\n{text}"
        );
        assert!(text.contains("0.8"));
        assert_eq!(Config::load(&p, None).pack, "aurora");
    }

    #[test]
    fn editing_twice_updates_rather_than_duplicates() {
        let (_d, p) = home();
        set(&p.config_file, "volume", "0.3").unwrap();
        set(&p.config_file, "volume", "0.9").unwrap();
        assert_eq!(read(&p).matches("volume").count(), 1, "{}", read(&p));
        assert_eq!(Config::load(&p, None).volume, 0.9);
    }

    #[test]
    fn every_settable_key_round_trips_through_the_loader() {
        // Guards the whole surface: if `set` writes something `load` rejects,
        // the user gets a config that silently does nothing.
        let cases = [
            ("pack", "cipher"),
            ("volume", "0.25"),
            ("enabled", "false"),
            ("policy.min_turn_seconds", "7"),
            ("policy.rate_limit_ms", "250"),
            ("policy.always_alert", "needs-you, failed"),
            ("policy.quiet_hours", "23:00-08:00"),
            ("policy.quiet_hours_action", "volume:0.2"),
            ("identity.per_project", "false"),
            ("remote.mode", "both"),
            ("remote.sequences", "bel, osc777"),
            ("events.tool-failed", "true"),
            ("events.done", "false"),
        ];
        for (key, value) in cases {
            let (_d, p) = home();
            set(&p.config_file, key, value).unwrap_or_else(|e| panic!("{key}={value}: {e}"));
            let loaded = Config::load_verbose(&p, None);
            assert!(
                loaded.warnings.is_empty(),
                "{key}={value} produced warnings: {:?}\n{}",
                loaded.warnings,
                read(&p)
            );
        }
    }

    #[test]
    fn values_are_written_with_the_right_toml_type() {
        let (_d, p) = home();
        set(&p.config_file, "volume", "0.25").unwrap();
        set(&p.config_file, "enabled", "false").unwrap();
        set(&p.config_file, "policy.min_turn_seconds", "7").unwrap();
        let text = read(&p);
        assert!(text.contains("volume = 0.25"), "{text}");
        assert!(text.contains("enabled = false"), "{text}");
        assert!(text.contains("min_turn_seconds = 7"), "{text}");
        assert!(
            !text.contains("\"7\""),
            "numbers must not be quoted:\n{text}"
        );
    }

    #[test]
    fn an_unknown_key_is_refused_with_the_valid_list() {
        let (_d, p) = home();
        let err = set(&p.config_file, "volumee", "0.5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("volumee"));
        assert!(err.contains("volume"), "should list the real key: {err}");
        assert!(!p.config_file.exists(), "nothing should have been written");
    }

    #[test]
    fn a_badly_typed_value_is_refused_before_writing() {
        let (_d, p) = home();
        for (key, bad) in [
            ("volume", "loud"),
            ("volume", "5"),
            ("enabled", "maybe"),
            ("policy.min_turn_seconds", "soon"),
            ("policy.quiet_hours", "23h-8h"),
            ("policy.quiet_hours_action", "shout"),
            ("policy.always_alert", "needsyou"),
            ("remote.mode", "sideways"),
            ("remote.sequences", "smoke-signal"),
            ("events.done", "sometimes"),
        ] {
            let err = set(&p.config_file, key, bad);
            assert!(err.is_err(), "{key}={bad} should have been refused");
            assert!(!p.config_file.exists(), "{key}={bad} wrote something");
        }
    }

    #[test]
    fn a_malformed_config_is_refused_rather_than_overwritten() {
        let (_d, p) = home();
        std::fs::create_dir_all(p.config_file.parent().unwrap()).unwrap();
        std::fs::write(&p.config_file, "this is not {{{ toml").unwrap();
        let err = set(&p.config_file, "volume", "0.5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Refusing"), "{err}");
        assert_eq!(
            read(&p),
            "this is not {{{ toml",
            "the file must be untouched"
        );
    }

    #[test]
    fn the_settable_key_list_covers_every_event() {
        let keys = settable_keys();
        for state in crate::core::event::State::ALL {
            assert!(
                keys.contains(&format!("events.{state}")),
                "missing events.{state}"
            );
        }
        assert!(keys.contains(&"pack".to_string()));
        assert!(keys.contains(&"policy.quiet_hours".to_string()));
        // Table names themselves are not settable.
        assert!(!keys.contains(&"policy".to_string()));
        assert!(!keys.contains(&"events".to_string()));
    }

    #[test]
    fn no_temp_files_are_left_behind() {
        let (_d, p) = home();
        set(&p.config_file, "pack", "cipher").unwrap();
        let strays: Vec<_> = std::fs::read_dir(p.config_file.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("beckon-tmp"))
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }
}
