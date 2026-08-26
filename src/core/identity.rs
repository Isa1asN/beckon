//! Per-project identity.
//!
//! Running several agents at once, a generic chime tells you *something* wants
//! you, not *which* thing. Transposing each project by a stable offset makes
//! `api-server` and `worktree-auth` audibly different without needing separate
//! packs — one parameter, and it costs nothing because the sounds are synthesised.

use std::path::Path;

/// Offsets, in semitones, drawn from a major hexatonic scale.
///
/// Every interval against every other is consonant, so two projects sounding at
/// once never clash — which is precisely the situation this exists for.
const OFFSETS: [f32; 6] = [0.0, 2.0, 4.0, 5.0, 7.0, 9.0];

/// The transposition for a project, or `0.0` when identity is switched off.
pub fn transpose_for(project: &Path, enabled: bool) -> f32 {
    if !enabled {
        return 0.0;
    }
    let hash = fnv1a(project.to_string_lossy().as_bytes());
    OFFSETS[(hash % OFFSETS.len() as u64) as usize]
}

/// FNV-1a, inlined. Public because anything needing a *stable* hash — one that
/// survives a toolchain upgrade — should use this rather than `DefaultHasher`.
///
/// `DefaultHasher` is explicitly not stable across Rust releases, and a project
/// that changes pitch after a toolchain upgrade would be a baffling bug report.
/// Six lines buys permanence.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn identity_can_be_switched_off() {
        assert_eq!(transpose_for(Path::new("/any/project"), false), 0.0);
    }

    #[test]
    fn the_same_project_always_gets_the_same_offset() {
        let p = Path::new("/home/dev/api-server");
        let first = transpose_for(p, true);
        for _ in 0..100 {
            assert_eq!(transpose_for(p, true), first);
        }
    }

    #[test]
    fn every_offset_comes_from_the_scale() {
        for i in 0..500 {
            let path = PathBuf::from(format!("/home/dev/project-{i}"));
            let offset = transpose_for(&path, true);
            assert!(OFFSETS.contains(&offset), "{offset} is not in the scale");
        }
    }

    #[test]
    fn different_projects_usually_differ() {
        // Not a guarantee for any given pair — six buckets — but the spread
        // should cover the scale rather than collapsing onto one value.
        let offsets: std::collections::BTreeSet<_> = (0..200)
            .map(|i| transpose_for(Path::new(&format!("/home/dev/repo-{i}")), true).to_bits())
            .collect();
        assert_eq!(
            offsets.len(),
            OFFSETS.len(),
            "hash does not spread across the scale"
        );
    }

    #[test]
    fn sibling_worktrees_are_distinguished() {
        // The exact case this feature exists for: same parent, different leaf.
        let a = transpose_for(Path::new("/home/dev/wt/feature-auth"), true);
        let b = transpose_for(Path::new("/home/dev/wt/feature-billing"), true);
        let c = transpose_for(Path::new("/home/dev/wt/feature-search"), true);
        assert!(
            a != b || b != c || a != c,
            "three sibling worktrees all landed on the same offset"
        );
    }

    #[test]
    fn the_hash_is_pinned_so_a_project_never_changes_pitch() {
        // Guards against swapping in a different hash: these values must hold
        // across Rust releases and beckon versions.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"hello"), 0xa430_d846_80aa_bd0b);
    }

    #[test]
    fn an_empty_path_still_yields_a_valid_offset() {
        assert!(OFFSETS.contains(&transpose_for(Path::new(""), true)));
    }
}
