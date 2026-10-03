//! `beckon test` — hear a pack without waiting for an agent to do something.

use crate::audio::out;
use crate::cli::play;
use crate::core::config::Config;
use crate::core::event::State;
use crate::core::identity;
use crate::core::paths::{self, Paths};
use crate::pack::resolve::{resolve_with_overrides, Source};
use crate::pack::store;
use crate::remote;

/// Gap between sounds, so a run of nine is legible rather than a smear.
const GAP: std::time::Duration = std::time::Duration::from_millis(450);

pub fn run(pack_id: Option<String>, only: Option<State>, here: bool) -> i32 {
    let paths = Paths::resolve();
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = paths::project_root(&cwd);
    let config = Config::load(&paths, Some(&root));

    let id = pack_id.unwrap_or_else(|| config.pack.clone());
    let Some((pack, origin)) = store::load(&paths, &id) else {
        eprintln!(
            "no pack named `{}`. Try `beckon packs` to see what is available.",
            crate::cli::safe(&id)
        );
        return 1;
    };

    let transpose = if here {
        identity::transpose_for(&root, config.identity.per_project)
    } else {
        0.0
    };

    println!(
        "{} — {}",
        crate::cli::safe(&pack.meta.name),
        crate::cli::safe(&pack.meta.description)
    );
    println!(
        "  pack {} ({origin:?}, {})",
        crate::cli::safe(&pack.meta.id),
        crate::cli::safe(&pack.meta.license)
    );
    if here {
        println!(
            "  as heard in {} (transposed {transpose:+} semitones)",
            crate::cli::safe_path(&root)
        );
    }
    println!();

    // Over SSH the remote speakers reach nobody; what reaches you is this
    // terminal, so that is what gets tested.
    let route = remote::route(config.remote.mode, remote::ssh_detected());

    let states: Vec<State> = match only {
        Some(state) => vec![state],
        None => State::ALL.to_vec(),
    };

    for (index, state) in states.iter().enumerate() {
        let Some((source_state, source)) = resolve_with_overrides(&pack, &config.sounds, *state)
        else {
            println!("  {state:<14} (silent — not defined)");
            continue;
        };

        let origin = match source {
            Source::File(path) => format!("  your file: {}", crate::cli::safe_path(path)),
            Source::Pack(_) if source_state == *state => String::new(),
            Source::Pack(_) => format!("  via {source_state}"),
        };

        let Some(pcm) = play::build(&pack, source, transpose) else {
            println!("  {state:<14} (could not be loaded — `beckon doctor` explains){origin}");
            continue;
        };

        println!("  {state:<14} {:>6.0}ms{origin}", pcm.duration_ms());

        if !route.local_audio() {
            continue;
        }
        let backend = out::play(&pcm, config.volume);
        if backend == out::Backend::Null {
            continue;
        }
        if index + 1 < states.len() {
            std::thread::sleep(GAP);
        }
    }

    if route.terminal() {
        send_to_terminal(
            &config,
            &root,
            only.unwrap_or(State::Done),
            route.local_audio(),
        );
    }
    0
}

/// Write one alert's escape sequences straight to this terminal, as the agent
/// would. Straight, because here nothing else is drawing on it.
fn send_to_terminal(config: &Config, root: &std::path::Path, state: State, also_local: bool) {
    use std::io::Write;
    println!();
    if config.remote.sequences.is_empty() {
        println!("  terminal  nothing to send — remote.sequences is empty");
        return;
    }
    println!(
        "  terminal  sent `{state}` as {} — watch for it on the machine you are typing on{}",
        remote::describe(&config.remote.sequences),
        if also_local {
            ""
        } else {
            " (no audio played here: remote)"
        }
    );
    let project = remote::project_label(root);
    let mut stdout = std::io::stdout().lock();
    let _ =
        stdout.write_all(remote::sequence(&config.remote.sequences, state, &project).as_bytes());
    let _ = stdout.flush();
}
