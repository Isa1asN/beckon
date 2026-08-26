//! `beckon test` — hear a pack without waiting for an agent to do something.

use crate::audio::out;
use crate::cli::play;
use crate::core::config::Config;
use crate::core::event::State;
use crate::core::identity;
use crate::core::paths::{self, Paths};
use crate::pack::resolve::{resolve_with_overrides, Source};
use crate::pack::store;

/// Gap between sounds, so a run of nine is legible rather than a smear.
const GAP: std::time::Duration = std::time::Duration::from_millis(450);

pub fn run(pack_id: Option<String>, only: Option<State>, here: bool) -> i32 {
    let paths = Paths::resolve();
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = paths::project_root(&cwd);
    let config = Config::load(&paths, Some(&root));

    let id = pack_id.unwrap_or_else(|| config.pack.clone());
    let Some((pack, origin)) = store::load(&paths, &id) else {
        eprintln!("no pack named `{id}`. Try `beckon packs` to see what is available.");
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
            root.display()
        );
    }
    println!();

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
            Source::File(path) => format!("  your file: {}", path.display()),
            Source::Pack(_) if source_state == *state => String::new(),
            Source::Pack(_) => format!("  via {source_state}"),
        };

        let Some(pcm) = play::build(&pack, source, transpose) else {
            println!("  {state:<14} (could not be loaded — `beckon doctor` explains){origin}");
            continue;
        };

        println!("  {state:<14} {:>6.0}ms{origin}", pcm.duration_ms());

        let backend = out::play(&pcm, config.volume);
        if backend == out::Backend::Null {
            continue;
        }
        if index + 1 < states.len() {
            std::thread::sleep(GAP);
        }
    }
    0
}
