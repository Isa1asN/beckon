//! Where beckon keeps its things.
//!
//! `BECKON_HOME` overrides every path. That is not a convenience — it is what
//! lets the test suite run without ever touching a developer's real config,
//! and what makes `beckon doctor` output reproducible.

use std::path::{Path, PathBuf};

/// Every filesystem location beckon uses, resolved once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// User-level `config.toml`.
    pub config_file: PathBuf,
    pub state_dir: PathBuf,
    pub data_dir: PathBuf,
    /// Installed sound packs, one directory each.
    pub packs_dir: PathBuf,
    /// One file per session, so parallel agents never contend on a shared file.
    pub sessions_dir: PathBuf,
    /// Timestamp until which beckon stays silent.
    pub mute_file: PathBuf,
    /// Where `beckon init` copies agent config before editing it.
    pub backup_dir: PathBuf,
}

impl Paths {
    /// Resolve from the environment, honouring `BECKON_HOME`.
    pub fn resolve() -> Paths {
        let home = std::env::var_os("BECKON_HOME").map(PathBuf::from);
        Paths::resolve_with(home.as_deref())
    }

    /// Resolve with an explicit override. `None` uses platform conventions.
    pub fn resolve_with(beckon_home: Option<&Path>) -> Paths {
        let (config_root, state_dir, data_dir) = match beckon_home {
            Some(home) => (home.to_path_buf(), home.join("state"), home.join("data")),
            None => platform_dirs(),
        };
        Paths {
            config_file: config_root.join("config.toml"),
            packs_dir: data_dir.join("packs"),
            sessions_dir: state_dir.join("sessions"),
            mute_file: state_dir.join("mute"),
            backup_dir: state_dir.join("backups"),
            state_dir,
            data_dir,
        }
    }
}

/// `(config, state, data)` per platform convention.
///
/// `state_dir` only exists on Linux; elsewhere it collapses into the data dir.
/// If the platform cannot tell us where home is, degrade to `~/.beckon` and
/// then to `./.beckon` rather than panicking — a hook must never die over this.
fn platform_dirs() -> (PathBuf, PathBuf, PathBuf) {
    if let Some(d) = directories::ProjectDirs::from("", "", "beckon") {
        let state = d.state_dir().unwrap_or_else(|| d.data_dir()).to_path_buf();
        return (
            d.config_dir().to_path_buf(),
            state,
            d.data_dir().to_path_buf(),
        );
    }
    let root = directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".beckon"))
        .unwrap_or_else(|| PathBuf::from(".beckon"));
    (root.clone(), root.join("state"), root.join("data"))
}

/// Markers that identify the root of a project.
///
/// `.beckon.toml` first so a project can opt in without being a repository.
const ROOT_MARKERS: [&str; 5] = [".beckon.toml", ".git", ".jj", ".hg", ".svn"];

/// How far up to look before giving up.
///
/// `cwd` arrives in the hook payload, and walking it unbounded costs five
/// `stat` calls plus a path join per level — quadratic in the path's length.
/// A 400 000-component `cwd` took 63 seconds *inside the hook process*, which
/// blocks the agent. Real trees are tens of levels deep, never hundreds.
const MAX_ANCESTORS: usize = 64;

/// Walk up from the agent's working directory to the project root.
///
/// Agents are routinely launched from a subdirectory, and a `.beckon.toml` at
/// the repository root must apply there too — silently ignoring it is the worst
/// kind of failure, because the user believes they configured something.
///
/// Nearest marker wins, so a nested repository is its own project. Falls back to
/// the starting directory when nothing is found.
pub fn project_root(from: &Path) -> PathBuf {
    for dir in from.ancestors().take(MAX_ANCESTORS) {
        if ROOT_MARKERS.iter().any(|marker| dir.join(marker).exists()) {
            return dir.to_path_buf();
        }
    }
    from.to_path_buf()
}

/// Markers that identify a *repository* root, as opposed to a project root.
const VCS_MARKERS: [&str; 4] = [".git", ".jj", ".hg", ".svn"];

/// The repository containing `from`, for deciding where `--scope project` writes.
///
/// Deliberately stricter than [`project_root`]: it ignores `.beckon.toml`,
/// which anyone can plant in a shared parent directory, and it refuses to
/// return the home directory. People keep dotfiles in a git repo at `$HOME`,
/// and "project scope" quietly resolving to `~/.claude/settings.json` — the
/// user-scope file — is the opposite of what was asked for.
pub fn vcs_root(from: &Path) -> Option<PathBuf> {
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    for dir in from.ancestors().take(MAX_ANCESTORS) {
        if Some(dir) == home.as_deref() {
            return None;
        }
        if VCS_MARKERS.iter().any(|marker| dir.join(marker).exists()) {
            return Some(dir.to_path_buf());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn under(root: &str) -> Paths {
        Paths::resolve_with(Some(Path::new(root)))
    }

    #[test]
    fn beckon_home_roots_everything() {
        let p = under("/tmp/bh");
        assert_eq!(p.config_file, Path::new("/tmp/bh/config.toml"));
        assert_eq!(p.state_dir, Path::new("/tmp/bh/state"));
        assert_eq!(p.data_dir, Path::new("/tmp/bh/data"));
        assert_eq!(p.sessions_dir, Path::new("/tmp/bh/state/sessions"));
        assert_eq!(p.mute_file, Path::new("/tmp/bh/state/mute"));
        assert_eq!(p.backup_dir, Path::new("/tmp/bh/state/backups"));
        assert_eq!(p.packs_dir, Path::new("/tmp/bh/data/packs"));
    }

    #[test]
    fn platform_paths_are_used_without_the_override() {
        // Assert shape rather than location, so this holds on Linux, macOS and
        // Windows alike.
        let p = Paths::resolve_with(None);
        assert!(p.config_file.is_absolute());
        assert!(p.config_file.ends_with("config.toml"));
        assert!(p.sessions_dir.ends_with("sessions"));
        assert!(p.packs_dir.ends_with("packs"));
        assert!(p.config_file.to_string_lossy().contains("beckon"));
    }

    #[test]
    fn derived_paths_stay_inside_their_parents() {
        let p = under("/tmp/bh");
        assert!(p.sessions_dir.starts_with(&p.state_dir));
        assert!(p.mute_file.starts_with(&p.state_dir));
        assert!(p.backup_dir.starts_with(&p.state_dir));
        assert!(p.packs_dir.starts_with(&p.data_dir));
    }

    #[test]
    fn resolution_is_deterministic() {
        assert_eq!(under("/tmp/bh"), under("/tmp/bh"));
        assert_eq!(Paths::resolve_with(None), Paths::resolve_with(None));
    }

    #[test]
    fn project_root_is_found_from_a_subdirectory() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("src/deep/deeper")).unwrap();
        std::fs::write(root.join(".beckon.toml"), "").unwrap();
        assert_eq!(project_root(&root.join("src/deep/deeper")), root);
        assert_eq!(project_root(root), root);
    }

    #[test]
    fn a_vcs_directory_also_marks_a_root() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("crates/inner")).unwrap();
        assert_eq!(project_root(&root.join("crates/inner")), root);
    }

    #[test]
    fn the_nearest_marker_wins() {
        let d = tempfile::tempdir().unwrap();
        let outer = d.path();
        let inner = outer.join("vendor/nested");
        std::fs::create_dir_all(inner.join(".git")).unwrap();
        std::fs::create_dir_all(outer.join(".git")).unwrap();
        assert_eq!(project_root(&inner), inner);
    }

    #[test]
    fn with_no_markers_the_starting_directory_is_the_root() {
        let d = tempfile::tempdir().unwrap();
        let deep = d.path().join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        // A temp dir under /tmp has no markers above it.
        assert_eq!(project_root(&deep), deep);
    }

    #[test]
    fn an_absurdly_deep_path_does_not_stall_the_hook() {
        // `cwd` comes from the hook payload, and the walk used to be unbounded:
        // 400k components cost 63 seconds in a hook that blocks the agent.
        let deep = PathBuf::from("/".to_string() + &"a/".repeat(200_000));

        // Generous on purpose. The bug this guards took 22 seconds at this
        // depth, and Windows needs a few hundred milliseconds just to walk
        // paths this long. Five seconds separates "bounded" from "quadratic"
        // without being sensitive to how fast the runner is.
        let started = std::time::Instant::now();
        let root = project_root(&deep);
        assert!(
            started.elapsed().as_secs() < 5,
            "took {:?}",
            started.elapsed()
        );
        assert!(root.is_absolute());

        let started = std::time::Instant::now();
        let _ = vcs_root(&deep);
        assert!(
            started.elapsed().as_secs() < 5,
            "vcs_root took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_repository_root_ignores_a_plantable_marker() {
        // `.beckon.toml` is fine for finding config, but must not decide where
        // an install writes — anyone can leave one in a shared parent.
        let d = tempfile::tempdir().unwrap();
        let outer = d.path();
        std::fs::write(outer.join(".beckon.toml"), "").unwrap();
        let inner = outer.join("repo/src");
        std::fs::create_dir_all(outer.join("repo/.git")).unwrap();
        std::fs::create_dir_all(&inner).unwrap();

        assert_eq!(project_root(&inner), outer.join("repo"));
        assert_eq!(vcs_root(&inner), Some(outer.join("repo")));

        // With no repository at all, there is no project to write into.
        let bare = d.path().join("bare/deep");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(vcs_root(&bare), None);
    }

    #[test]
    fn a_relative_beckon_home_is_still_usable() {
        let p = Paths::resolve_with(Some(Path::new("rel")));
        assert_eq!(p.config_file, Path::new("rel/config.toml"));
    }
}
