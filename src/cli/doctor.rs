//! `beckon doctor` — why isn't it making a sound?
//!
//! Silence has many legitimate causes, and a user cannot tell them apart from a
//! bug. This prints every one of them.

use crate::adapter;
use crate::audio::out;
use crate::core::config::{Config, QuietAction};
use crate::core::event::State;
use crate::core::paths::{self, Paths};
use crate::core::{identity, state};
use crate::pack::{resolve::resolve, store};
use chrono::Local;

pub fn run() -> i32 {
    let paths = Paths::resolve();
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = paths::project_root(&cwd);
    let loaded = Config::load_verbose(&paths, Some(&root));
    let config = &loaded.config;

    println!("beckon {}", env!("CARGO_PKG_VERSION"));
    println!();

    println!("paths");
    println!("  config    {}", paths.config_file.display());
    println!("  state     {}", paths.state_dir.display());
    println!("  packs     {}", paths.packs_dir.display());
    if std::env::var_os("BECKON_HOME").is_some() {
        println!("  (overridden by BECKON_HOME)");
    }
    println!();

    println!("project");
    println!("  cwd       {}", cwd.display());
    println!("  root      {}", root.display());
    let transpose = identity::transpose_for(&root, config.identity.per_project);
    if config.identity.per_project {
        println!("  identity  transposed {transpose:+} semitones");
    } else {
        println!("  identity  off");
    }
    println!();

    println!("sound");
    match store::load(&paths, &config.pack) {
        Some((pack, origin)) => {
            println!(
                "  pack      {} ({origin:?})",
                crate::cli::safe(&pack.meta.id)
            );
            let missing: Vec<String> = State::ALL
                .into_iter()
                .filter(|s| resolve(&pack, *s).is_none())
                .map(|s| s.to_string())
                .collect();
            if missing.is_empty() {
                println!("  coverage  all nine states");
            } else {
                println!("  coverage  silent for: {}", missing.join(", "));
            }
        }
        None => println!(
            "  pack      `{}` NOT FOUND — beckon will be silent",
            config.pack
        ),
    }
    println!("  volume    {:.2}", config.volume);

    if config.sounds.is_empty() {
        println!("  yours     none — set with `beckon config set` or a [sounds] table");
    } else {
        println!(
            "  yours     {} override(s) from your config:",
            config.sounds.len()
        );
        for (state, path) in &config.sounds {
            // Report why a file will not play *here*, where someone is already
            // asking why it is quiet.
            let status = match crate::audio::sample::load(path) {
                Ok(pcm) => format!("{:.0}ms", pcm.duration_ms()),
                Err(e) => format!("BROKEN — {e}"),
            };
            println!("            {state:<14} {status}");
            println!("            {:<14} {}", "", path.display());
        }
    }

    match out::override_from_env() {
        Some(backend) => println!("  backend   {backend} (forced by BECKON_AUDIO)"),
        None => {
            let embedded = cfg!(feature = "embedded-audio");
            let system = out::available_system_player();
            let chosen = if embedded {
                "embedded".to_string()
            } else if let Some(program) = system {
                format!("system player ({program})")
            } else {
                "terminal bell".to_string()
            };
            println!("  backend   {chosen}");
            if !embedded {
                println!("            (built without the embedded-audio feature)");
            }
            for (program, present) in out::system_player_report() {
                println!(
                    "            {} {program}",
                    if present { "found  " } else { "missing" }
                );
            }
        }
    }
    println!();

    println!("policy");
    println!("  enabled   {}", config.enabled);
    match state::read_mute(&paths) {
        Some(until) if until > chrono::Utc::now() => {
            println!(
                "  muted     until {}",
                until.with_timezone(&Local).format("%H:%M:%S")
            );
        }
        _ => println!("  muted     no"),
    }
    match &config.policy.quiet_hours {
        Some(window) => {
            let now = Local::now().time();
            let inside = window.contains(now);
            let action = match config.policy.quiet_hours_action {
                QuietAction::Silence => "silence".to_string(),
                QuietAction::Volume(v) => format!("volume {v:.2}"),
            };
            println!(
                "  quiet     {}-{} ({action}) — currently {}",
                window.start.format("%H:%M"),
                window.end.format("%H:%M"),
                if inside { "INSIDE" } else { "outside" }
            );
        }
        None => println!("  quiet     not configured"),
    }
    println!(
        "  gate      done stays silent under {}s",
        config.policy.min_turn_seconds
    );
    println!(
        "  repeat    same sound, same session, within {}ms",
        config.policy.rate_limit_ms
    );
    let off: Vec<String> = State::ALL
        .into_iter()
        .filter(|s| !config.events.enabled(*s))
        .map(|s| s.to_string())
        .collect();
    if !off.is_empty() {
        println!("  disabled  {}", off.join(", "));
    }
    println!();

    println!("agents");
    for id in adapter::KNOWN_AGENTS {
        let installed = which(id);
        println!(
            "  {:<12} {}",
            id,
            match installed {
                Some(path) => format!("found at {path}"),
                None => "not installed".to_string(),
            }
        );
    }
    println!("  hooks     run `beckon init` to bind them");
    println!();

    if !loaded.warnings.is_empty() {
        println!("warnings");
        for warning in &loaded.warnings {
            println!("  {}", crate::cli::safe(warning));
        }
        println!();
    }

    println!("debugging");
    println!("  BECKON_TRACE=/tmp/beckon.log   log every decision");
    println!("  BECKON_DUMP=/tmp/hooks.jsonl   capture raw hook payloads");
    println!("  BECKON_AUDIO=null              force silence");
    println!("  beckon test                    hear the active pack");

    0
}

/// Locate an agent binary on `PATH`.
fn which(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|found| found.display().to_string())
}
