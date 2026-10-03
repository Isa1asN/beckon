//! Finding packs, wherever they live.
//!
//! Installed packs shadow built-ins of the same id, so someone can iterate on a
//! fork of `aurora` without renaming it.

use crate::core::files;
use crate::core::paths::Paths;
use crate::pack::{builtin, manifest::Pack};

/// Where a pack came from. Shown by `beckon packs` and `beckon doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Builtin,
    Installed,
}

pub const MANIFEST_NAME: &str = "pack.toml";

/// Load a pack by id: installed first, then built-in.
pub fn load(paths: &Paths, id: &str) -> Option<(Pack, Origin)> {
    if let Some(pack) = load_installed(paths, id) {
        return Some((pack, Origin::Installed));
    }
    builtin::get(id).map(|p| (p, Origin::Builtin))
}

fn load_installed(paths: &Paths, id: &str) -> Option<Pack> {
    // The id reaches us from config, so keep it from walking the filesystem.
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let dir = paths.packs_dir.join(id);
    // Bounded and regular-file-only: a manifest that is a FIFO hung `packs`,
    // and one linked to /dev/zero read until memory ran out — in the detached
    // player, too, before it had claimed a slot.
    let text = files::read_bounded(&dir.join(MANIFEST_NAME), files::limit::PACK_MANIFEST).ok()?;
    let mut pack = Pack::parse(&text, Some(dir)).ok()?;
    // A pack is its directory. Its manifest's id is overruled rather than
    // trusted: otherwise a pack in `zzfake/` saying `id = "aurora"` listed as
    // a second `aurora`, active marker and all. A copy of `aurora` made into
    // `aurora2/` without editing it simply is `aurora2`.
    pack.meta.id = id.to_string();
    Some(pack)
}

/// Every available pack, built-ins and installed, sorted by id, with installed
/// shadowing built-ins.
pub fn list(paths: &Paths) -> Vec<(Pack, Origin)> {
    let mut by_id: std::collections::BTreeMap<String, (Pack, Origin)> = builtin::all()
        .into_iter()
        .map(|p| (p.meta.id.clone(), (p, Origin::Builtin)))
        .collect();

    if let Ok(entries) = std::fs::read_dir(&paths.packs_dir) {
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if let Some(pack) = load_installed(paths, &id) {
                by_id.insert(id, (pack, Origin::Installed));
            }
        }
    }
    by_id.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::resolve_with(Some(d.path()));
        (d, p)
    }

    fn install(paths: &Paths, id: &str, name: &str) {
        let dir = paths.packs_dir.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(MANIFEST_NAME),
            format!(
                "[pack]\nid=\"{id}\"\nname=\"{name}\"\nversion=\"1\"\nauthor=\"a\"\n\
                 license=\"CC0-1.0\"\n[sounds.done]\ntype=\"synth\"\n\
                 [[sounds.done.layer]]\nwave=\"sine\"\nnotes=[440.0]\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn builtins_load_without_anything_installed() {
        let (_d, p) = home();
        let (pack, origin) = load(&p, "aurora").unwrap();
        assert_eq!(pack.meta.id, "aurora");
        assert_eq!(origin, Origin::Builtin);
    }

    #[test]
    fn an_unknown_id_is_none() {
        let (_d, p) = home();
        assert!(load(&p, "nope").is_none());
    }

    #[test]
    fn an_installed_pack_loads_and_remembers_its_directory() {
        let (_d, p) = home();
        install(&p, "mine", "Mine");
        let (pack, origin) = load(&p, "mine").unwrap();
        assert_eq!(pack.meta.name, "Mine");
        assert_eq!(origin, Origin::Installed);
        assert_eq!(
            pack.root,
            Some(p.packs_dir.join("mine")),
            "samples resolve against this"
        );
    }

    #[test]
    fn an_installed_pack_shadows_a_builtin_of_the_same_id() {
        let (_d, p) = home();
        install(&p, "aurora", "My Aurora");
        let (pack, origin) = load(&p, "aurora").unwrap();
        assert_eq!(pack.meta.name, "My Aurora");
        assert_eq!(origin, Origin::Installed);
    }

    #[test]
    fn a_pack_is_its_directory_whatever_its_manifest_claims() {
        // A copy of a pack left with the original's id still works under its
        // own name, and cannot pose as the original in the listing.
        let (_d, p) = home();
        install(&p, "aurora", "My Aurora");
        std::fs::rename(p.packs_dir.join("aurora"), p.packs_dir.join("aurora2")).unwrap();

        let (pack, origin) = load(&p, "aurora2").unwrap();
        assert_eq!(pack.meta.id, "aurora2");
        assert_eq!(origin, Origin::Installed);

        let listed: Vec<(String, Origin)> = list(&p)
            .into_iter()
            .map(|(pack, origin)| (pack.meta.id, origin))
            .collect();
        assert!(
            listed.contains(&("aurora".to_string(), Origin::Builtin)),
            "{listed:?}"
        );
        assert!(
            listed.contains(&("aurora2".to_string(), Origin::Installed)),
            "{listed:?}"
        );
        assert_eq!(listed.iter().filter(|(id, _)| id == "aurora").count(), 1);
    }

    #[test]
    fn a_pack_id_cannot_walk_the_filesystem() {
        let (_d, p) = home();
        for hostile in ["../../etc", "..", "a/b", "a\\b", "", "a b"] {
            assert!(load(&p, hostile).is_none(), "{hostile:?} was accepted");
        }
    }

    #[test]
    fn a_broken_installed_pack_is_skipped_rather_than_crashing() {
        let (_d, p) = home();
        let dir = p.packs_dir.join("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_NAME), "{{{ not toml").unwrap();
        assert!(load(&p, "broken").is_none());
        // And it must not take the listing down with it.
        assert_eq!(list(&p).len(), 3, "built-ins should still list");
    }

    #[test]
    fn listing_covers_builtins_and_installed_together() {
        let (_d, p) = home();
        install(&p, "mine", "Mine");
        let ids: Vec<String> = list(&p).into_iter().map(|(pack, _)| pack.meta.id).collect();
        assert_eq!(ids, vec!["aurora", "cipher", "mine", "unit-7"]);
    }

    #[test]
    fn listing_marks_a_shadowed_builtin_as_installed() {
        let (_d, p) = home();
        install(&p, "cipher", "Forked Cipher");
        let found = list(&p)
            .into_iter()
            .find(|(pack, _)| pack.meta.id == "cipher")
            .unwrap();
        assert_eq!(found.1, Origin::Installed);
        assert_eq!(list(&p).len(), 3, "shadowing must not duplicate the entry");
    }

    #[test]
    fn listing_a_missing_packs_directory_still_returns_builtins() {
        let (_d, p) = home();
        assert_eq!(list(&p).len(), 3);
    }
}
