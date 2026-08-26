//! `beckon packs` and `beckon use`.

use crate::core::config::Config;
use crate::core::config_edit;
use crate::core::event::State;
use crate::core::paths::{self, Paths};
use crate::pack::{resolve::resolve, store};

pub fn list() -> i32 {
    let paths = Paths::resolve();
    let cwd = std::env::current_dir().unwrap_or_default();
    let config = Config::load(&paths, Some(&paths::project_root(&cwd)));

    let packs = store::list(&paths);
    if packs.is_empty() {
        println!("No packs found. That should not happen — the built-ins ship inside the binary.");
        return 1;
    }

    for (pack, origin) in &packs {
        let active = if pack.meta.id == config.pack {
            "*"
        } else {
            " "
        };
        let silent = State::ALL
            .into_iter()
            .filter(|s| resolve(pack, *s).is_none())
            .count();
        let coverage = if silent == 0 {
            String::new()
        } else {
            format!("  ({silent} silent)")
        };
        println!(
            "{active} {:<10} {:<9} {}{coverage}",
            crate::cli::safe(&pack.meta.id),
            format!("{origin:?}").to_lowercase(),
            crate::cli::safe(&pack.meta.description)
        );
    }
    println!();
    println!("(*) is active. `beckon use <id>` to switch, `beckon test <id>` to hear one first.");
    0
}

pub fn use_pack(id: &str) -> i32 {
    let paths = Paths::resolve();

    // Check before writing: a config pointing at a pack that does not exist
    // makes beckon silent, and silence is indistinguishable from a bug.
    let Some((pack, _)) = store::load(&paths, id) else {
        eprintln!("no pack named `{id}`.");
        eprintln!();
        eprintln!("Available:");
        for (pack, _) in store::list(&paths) {
            eprintln!("  {}", crate::cli::safe(&pack.meta.id));
        }
        return 1;
    };

    if let Err(e) = config_edit::set(&paths.config_file, "pack", id) {
        eprintln!("{e}");
        return 1;
    }

    println!(
        "Now using {} — {}",
        crate::cli::safe(&pack.meta.name),
        crate::cli::safe(&pack.meta.description)
    );
    let silent: Vec<String> = State::ALL
        .into_iter()
        .filter(|s| resolve(&pack, *s).is_none())
        .map(|s| s.to_string())
        .collect();
    if !silent.is_empty() {
        println!("  Silent for: {}", silent.join(", "));
    }
    println!("  `beckon test` to hear it.");
    0
}
