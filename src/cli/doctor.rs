//! `beckon doctor` — why isn't it making a sound?
//!
//! Silence has many legitimate causes, and a user cannot tell them apart from a
//! bug. This prints every one of them.

use crate::adapter::{self, Scope};
use crate::audio::out;
use crate::core::config::{Config, QuietAction};
use crate::core::event::State;
use crate::core::paths::{self, Paths};
use crate::core::{identity, state};
use crate::pack::{resolve::resolve, store};
use crate::remote::{self, Route};
use crate::settings_json;
use chrono::Local;
use std::path::{Path, PathBuf};

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

    println!("remote");
    let ssh = remote::ssh_detected();
    let route = remote::route(config.remote.mode, ssh);
    let mode = format!("{:?}", config.remote.mode).to_lowercase();
    println!(
        "  mode      {mode} — {}",
        if ssh {
            "over SSH (SSH_CONNECTION or SSH_TTY is set)"
        } else {
            "not over SSH"
        }
    );
    let names = remote::describe(&config.remote.sequences);
    match route {
        Route::Local => println!("  alerts    played here"),
        _ if config.remote.sequences.is_empty() => println!(
            "  alerts    {} — remote.sequences is empty, so nothing reaches your terminal",
            if route.local_audio() {
                "played here only"
            } else {
                "SILENT"
            }
        ),
        Route::Terminal => {
            println!("  alerts    sent to your terminal as {names}; not played here")
        }
        Route::Both => println!("  alerts    sent to your terminal as {names}, and played here"),
    }
    if route.terminal() {
        println!("            never for session-start or compacting: their hook output reaches the model");
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
    let gated: Vec<String> = State::ALL
        .into_iter()
        .filter(|s| config.events.enabled(*s) && !config.policy.always_alert.contains(s))
        .map(|s| s.to_string())
        .collect();
    if !gated.is_empty() {
        println!(
            "  gate      {} {} silent within {}s of your last prompt, or of {} own last play",
            gated.join(", "),
            if gated.len() == 1 { "stays" } else { "stay" },
            config.policy.min_turn_seconds,
            if gated.len() == 1 { "its" } else { "their" },
        );
    }
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
    report_hooks(&cwd);
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

/// Where the hooks are bound, and to which beckon.
///
/// This used to print "run `beckon init`" unconditionally, whether or not that
/// had been done — the one question `doctor` exists to answer. And a binding to
/// some *other* beckon is the quiet failure worth catching: an old build left
/// on `PATH` keeps running every hook while you test the new one.
fn report_hooks(cwd: &Path) {
    let Some(adapter) = adapter::adapter_for("claude-code") else {
        return;
    };
    let expected: Vec<&str> = adapter
        .install_plan("")
        .bindings
        .iter()
        .map(|b| b.event)
        .collect();

    let mut scopes = vec![(Scope::User, cwd.to_path_buf())];
    if let Some(repo) = paths::vcs_root(cwd) {
        scopes.push((Scope::Project, repo));
    }

    let mut bound_anywhere = false;
    for (scope, root) in scopes {
        let Some(path) = adapter.settings_path(scope, &root) else {
            continue;
        };
        let settings = match crate::cli::install::read_settings(&path) {
            Ok(settings) => settings,
            Err(e) => {
                println!(
                    "  hooks     {} settings unreadable: {}",
                    scope.as_str(),
                    crate::cli::safe(&e)
                );
                continue;
            }
        };
        let bound = settings_json::beckon_bindings(&settings);
        if bound.is_empty() {
            continue;
        }
        bound_anywhere = true;

        let missing: Vec<&str> = expected
            .iter()
            .copied()
            .filter(|event| {
                !bound
                    .iter()
                    .any(|(_, events)| events.iter().any(|e| e == event))
            })
            .collect();
        let file = crate::cli::safe(&path.display().to_string());
        if missing.is_empty() {
            println!("  hooks     all {} bound in {file}", expected.len());
        } else {
            println!(
                "  hooks     {} of {} bound in {file} — missing {}; `beckon init` adds them",
                expected.len() - missing.len(),
                expected.len(),
                missing.join(", ")
            );
        }
        // The project file arrives with the repository. Claude Code will not
        // run its hooks until you trust the workspace, and doctor must not
        // run what it names either.
        let may_run = scope == Scope::User;
        for (command, _) in &bound {
            println!("            runs {}", crate::cli::safe(command));
            println!("                 {}", describe_program(command, may_run));
        }
    }
    if !bound_anywhere {
        println!("  hooks     not bound — run `beckon init`");
    }
}

/// Which beckon a hook command will actually run.
///
/// Execution is opt-in: asking a binary its version means running it, which is
/// fine for the user's own settings and not for a repository's.
fn describe_program(command: &str, may_run: bool) -> String {
    let Some(program) = settings_json::program_of(command) else {
        return "cannot tell which program this runs".to_string();
    };
    let found = match locate(program) {
        Located::Found(path) => path,
        Located::Missing => {
            return "NOT FOUND — this hook cannot run, so every sound is lost".to_string()
        }
        Located::Unknown(why) => return format!("cannot tell from here: {why}"),
    };

    let this = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    if this.is_some() && found.canonicalize().ok() == this {
        return "this binary".to_string();
    }
    let ours = env!("CARGO_PKG_VERSION");
    if !may_run {
        return format!(
            "{} — not this binary; not run to ask its version, as it comes from this repository's settings",
            crate::cli::safe(&found.display().to_string())
        );
    }
    match version_of(&found) {
        Some(theirs) if theirs == ours => {
            format!("a different copy of beckon {ours} — not this binary")
        }
        Some(theirs) => format!("beckon {theirs} — not this binary, which is {ours}"),
        None => "did not report a version — not this binary".to_string(),
    }
}

enum Located {
    Found(PathBuf),
    Missing,
    /// Depends on something only the hook's shell knows.
    Unknown(&'static str),
}

/// Find the file a hook's program names, the way its shell would — or say
/// that we cannot, rather than guess. A wrong "NOT FOUND" sends someone
/// chasing a hook that works.
fn locate(program: &str) -> Located {
    let expanded = match program
        .strip_prefix("~/")
        .or_else(|| program.strip_prefix("~\\"))
    {
        Some(rest) => match directories::BaseDirs::new() {
            Some(dirs) => dirs.home_dir().join(rest),
            None => return Located::Unknown("it starts with ~ and there is no home directory"),
        },
        None if program.contains(['$', '%', '`']) => {
            return Located::Unknown("it uses shell expansion")
        }
        None => PathBuf::from(program),
    };

    if expanded.is_absolute() {
        return if expanded.is_file() {
            Located::Found(expanded)
        } else {
            Located::Missing
        };
    }
    if program.contains(std::path::is_separator) {
        return Located::Unknown("a relative path, resolved wherever the agent runs it");
    }
    match which(program) {
        Some(found) => Located::Found(PathBuf::from(found)),
        None => Located::Missing,
    }
}

/// Ask a beckon binary its version, giving up rather than hanging.
///
/// The deadline covers the whole exchange, output included: a wrapper script
/// can exit while something it started still holds the pipe open, and a read
/// waiting for that to close would never return.
fn version_of(program: &Path) -> Option<String> {
    let mut child = std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(std::time::Duration::from_secs(2));
    // A no-op if it already exited; otherwise it has had its chance.
    let _ = child.kill();
    let _ = child.wait();

    let version = out.ok()?.trim().strip_prefix("beckon ")?.to_string();
    Some(crate::cli::safe(&version))
}

/// Locate a program on `PATH`, as a shell would.
///
/// Only absolute entries: an empty or relative one means "the current
/// directory", which would let whatever repository you are standing in answer
/// for a program name. On Windows a bare name also matches `name.exe`.
fn which(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) && Path::new(program).extension().is_none() {
        vec![program.to_string(), format!("{program}.exe")]
    } else {
        vec![program.to_string()]
    };
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .map(|found| found.display().to_string())
}
