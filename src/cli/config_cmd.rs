//! `beckon config` — read and write settings.

use crate::core::config::{Config, QuietAction};
use crate::core::config_edit;
use crate::core::event::State;
use crate::core::paths::{self, Paths};

pub fn show() -> i32 {
    let paths = Paths::resolve();
    let cwd = std::env::current_dir().unwrap_or_default();
    let loaded = Config::load_verbose(&paths, Some(&paths::project_root(&cwd)));

    println!("# effective configuration");
    println!("# user   {}", paths.config_file.display());
    println!();
    for key in config_edit::settable_keys() {
        println!(
            "{key} = {}",
            get_value(&loaded.config, &key).unwrap_or_default()
        );
    }

    if !loaded.warnings.is_empty() {
        println!();
        for warning in &loaded.warnings {
            println!("# warning: {}", crate::cli::safe(warning));
        }
    }
    0
}

pub fn get(key: &str) -> i32 {
    let paths = Paths::resolve();
    let cwd = std::env::current_dir().unwrap_or_default();
    let config = Config::load(&paths, Some(&paths::project_root(&cwd)));

    match get_value(&config, key) {
        Some(value) => {
            println!("{value}");
            0
        }
        None => {
            eprintln!("unknown setting `{key}`");
            eprintln!();
            eprintln!("Valid settings:");
            for k in config_edit::settable_keys() {
                eprintln!("  {k}");
            }
            2
        }
    }
}

pub fn set(key: &str, value: &str) -> i32 {
    let paths = Paths::resolve();
    if let Err(e) = config_edit::set(&paths.config_file, key, value) {
        eprintln!("{e}");
        return 2;
    }

    // Read it back rather than trusting the write: this is the moment a bad
    // value would otherwise become mysterious silence.
    let loaded = Config::load_verbose(&paths, None);
    println!(
        "{key} = {}",
        get_value(&loaded.config, key).unwrap_or_default()
    );
    for warning in &loaded.warnings {
        eprintln!("warning: {}", crate::cli::safe(warning));
    }
    0
}

pub fn path() -> i32 {
    println!("{}", Paths::resolve().config_file.display());
    0
}

/// One dotted key's effective value, rendered as TOML.
fn get_value(config: &Config, key: &str) -> Option<String> {
    if let Some(name) = key.strip_prefix("events.") {
        let state = State::parse(name)?;
        return Some(config.events.enabled(state).to_string());
    }
    if let Some(name) = key.strip_prefix("sounds.") {
        let state = State::parse(name)?;
        return Some(match config.sounds.get(&state) {
            Some(path) => format!("{:?}", path.display().to_string()),
            None => "# not set".to_string(),
        });
    }

    Some(match key {
        "pack" => format!("{:?}", config.pack),
        "volume" => format!("{:.2}", config.volume),
        "enabled" => config.enabled.to_string(),
        "policy.min_turn_seconds" => config.policy.min_turn_seconds.to_string(),
        "policy.rate_limit_ms" => config.policy.rate_limit_ms.to_string(),
        "policy.always_alert" => format!(
            "[{}]",
            config
                .policy
                .always_alert
                .iter()
                .map(|s| format!("{:?}", s.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "policy.quiet_hours" => match &config.policy.quiet_hours {
            Some(w) => format!("\"{}-{}\"", w.start.format("%H:%M"), w.end.format("%H:%M")),
            None => "# not set".to_string(),
        },
        "policy.quiet_hours_action" => match config.policy.quiet_hours_action {
            QuietAction::Silence => "\"silence\"".to_string(),
            QuietAction::Volume(v) => format!("\"volume:{v}\""),
        },
        "identity.per_project" => config.identity.per_project.to_string(),
        "remote.mode" => format!("{:?}", format!("{:?}", config.remote.mode).to_lowercase()),
        "remote.sequences" => format!(
            "[{}]",
            config
                .remote
                .sequences
                .iter()
                .map(|s| format!("{:?}", format!("{s:?}").to_lowercase()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => return None,
    })
}
