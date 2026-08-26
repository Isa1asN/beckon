//! Small pieces of persistent state: when the current turn started, when this
//! session last played each sound, and whether the user has muted us.
//!
//! Three properties matter here.
//!
//! **One file per session.** Four agents in four worktrees write four different
//! files, so there is no shared-file contention and no locking.
//!
//! **Rate limiting is per session, per state.** A machine-wide throttle looks
//! reasonable until you run agents in parallel: their turn boundaries are
//! correlated, so one agent's completion chime swallows another's permission
//! alert. A *different* state always carries new information, and so does the
//! same state from a *different* agent. The only burst worth collapsing is the
//! same sound from the same session — `Stop` and `Notification/agent_completed`
//! both mapping to `done`, or a parallel tool batch producing several
//! `tool-failed`.
//!
//! **Every read fails soft.** A missing, truncated or corrupt file reads as
//! `None`, and every write swallows its errors. This state is an optimisation
//! for politeness; losing it must never cost more than an extra chime.

use crate::core::event::State;
use crate::core::paths::Paths;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Contents of `sessions/<id>.json`.
///
/// Both fields are optional so a session that plays a sound before ever seeing
/// a `UserPromptSubmit` still gets a valid file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn_started: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    last_played: BTreeMap<State, DateTime<Utc>>,
}

/// Longest session id we will keep. Real ids are UUIDs; anything longer is
/// either a bug or someone probing.
const MAX_ID_LEN: usize = 128;

/// Make a session id safe to use as a filename.
///
/// The id arrives inside an untrusted JSON payload. Rather than blacklisting
/// separators and dots — which invites encoding tricks — keep only characters
/// that cannot mean anything to a filesystem.
pub fn sanitize_session_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(MAX_ID_LEN)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

fn session_file(paths: &Paths, session_id: &str) -> PathBuf {
    paths
        .sessions_dir
        .join(format!("{}.json", sanitize_session_id(session_id)))
}

fn read_session(paths: &Paths, session_id: &str) -> SessionState {
    std::fs::read_to_string(session_file(paths, session_id))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// How long a lock may be held before we assume its owner died.
const LOCK_STALE: Duration = Duration::milliseconds(5_000);
/// Milliseconds to keep trying for a lock before giving up and failing open.
const LOCK_ATTEMPTS: u32 = 50;

/// An advisory lock built on exclusive create — the one operation every
/// filesystem performs atomically.
///
/// Needed because the whole point is parallel agents. Without it, N hooks
/// arriving together all read the same empty history before any of them writes,
/// so the rate limit cannot collapse a burst: twenty concurrent events at one
/// session produced up to nineteen simultaneous chimes. Serialising the
/// read-decide-write makes the first one play and the rest correctly suppress.
pub struct SessionLock {
    path: PathBuf,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Take the lock for one session, or `None` if it could not be had.
///
/// A caller that gets `None` should carry on regardless: an extra sound is a
/// far better failure than mysterious silence.
pub fn lock_session(paths: &Paths, session_id: &str) -> Option<SessionLock> {
    let target = paths
        .sessions_dir
        .join(format!("{}.json", sanitize_session_id(session_id)));
    lock_path(&target)
}

/// Take an advisory lock guarding `target`, or `None` if it could not be had.
///
/// Used anywhere a read-modify-write must not interleave: session state, and
/// `beckon config set`, where a lost update leaves beckon silent with no sign
/// that the change never landed.
pub fn lock_path(target: &Path) -> Option<SessionLock> {
    let path = target.with_extension("beckon-lock");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    for _ in 0..LOCK_ATTEMPTS {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Some(SessionLock { path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Break a lock whose owner died holding it.
                let abandoned = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map(|when| Utc::now() - DateTime::<Utc>::from(when) > LOCK_STALE)
                    .unwrap_or(false);
                if abandoned {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(_) => return None,
        }
    }
    None
}

/// Read, modify, write.
///
/// Callers that care about atomicity hold [`lock_session`] across the whole
/// read-decide-write; this alone is not enough.
fn update_session(paths: &Paths, session_id: &str, edit: impl FnOnce(&mut SessionState)) {
    let mut state = read_session(paths, session_id);
    edit(&mut state);
    let Ok(json) = serde_json::to_vec(&state) else {
        return;
    };
    atomic_write(&session_file(paths, session_id), &json);
}

/// Write via a temp file and rename, so a concurrent reader never sees a
/// half-written file. The temp name includes the target so two writes from the
/// same process to the same directory cannot collide.
fn atomic_write(path: &Path, bytes: &[u8]) {
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{}.{}.tmp", name, std::process::id()));
    if std::fs::write(&tmp, bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn read_ts(path: &Path) -> Option<DateTime<Utc>> {
    let raw = std::fs::read_to_string(path).ok()?;
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn write_ts(path: &Path, when: DateTime<Utc>) {
    atomic_write(path, when.to_rfc3339().as_bytes());
}

// ------------------------------------------------------------------ per session

/// Record that a turn just began. Backs the duration gate.
pub fn record_turn_start(paths: &Paths, session_id: &str, now: DateTime<Utc>) {
    update_session(paths, session_id, |s| s.turn_started = Some(now));
}

pub fn read_turn_start(paths: &Paths, session_id: &str) -> Option<DateTime<Utc>> {
    read_session(paths, session_id).turn_started
}

/// Record that this session just played `state`. Backs the rate limit.
pub fn record_played(paths: &Paths, session_id: &str, state: State, now: DateTime<Utc>) {
    update_session(paths, session_id, |s| {
        s.last_played.insert(state, now);
    });
}

/// When this session last played this particular sound.
pub fn read_last_played(paths: &Paths, session_id: &str, state: State) -> Option<DateTime<Utc>> {
    read_session(paths, session_id)
        .last_played
        .get(&state)
        .copied()
}

/// Forget one session, on `SessionEnd`.
pub fn prune_session(paths: &Paths, session_id: &str) {
    let _ = std::fs::remove_file(session_file(paths, session_id));
}

/// Most sounds allowed to play at once.
///
/// Nothing bounded this before: 240 rapid events produced 241 concurrent player
/// processes, each holding an audio device. Past a handful, additional voices
/// carry no information — they are just a wall of noise and a fork bomb's worth
/// of processes.
pub const MAX_CONCURRENT_PLAYERS: usize = 8;

/// A player slot is abandoned if held longer than this. Generously longer than
/// the longest sound beckon will render.
const SLOT_STALE: Duration = Duration::milliseconds(60_000);

/// Claim one of the playback slots, or `None` if all are busy.
///
/// The returned guard releases the slot when dropped, including on panic.
pub fn claim_player_slot(paths: &Paths) -> Option<SessionLock> {
    let dir = paths.state_dir.join("players");
    let _ = std::fs::create_dir_all(&dir);

    for slot in 0..MAX_CONCURRENT_PLAYERS {
        let path = dir.join(format!("slot-{slot}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Some(SessionLock { path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if held_longer_than(&path, SLOT_STALE) {
                    // Its owner died mid-sound; take it over.
                    let _ = std::fs::remove_file(&path);
                    if let Ok(_f) = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                    {
                        return Some(SessionLock { path });
                    }
                }
            }
            Err(_) => return None,
        }
    }
    None
}

/// Has this marker existed for longer than `limit`?
fn held_longer_than(path: &Path, limit: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|when| Utc::now() - DateTime::<Utc>::from(when) > limit)
        .unwrap_or(false)
}

/// Opportunistic garbage collection of abandoned sessions and stray temp files.
///
/// Runs on every turn start — one of the hooks that can block the agent — so it
/// uses `stat` rather than opening and parsing each file. Every write refreshes
/// mtime, so mtime is exactly "when this session was last active".
pub fn prune_older_than(paths: &Paths, now: DateTime<Utc>, days: i64) {
    let Ok(entries) = std::fs::read_dir(&paths.sessions_dir) else {
        return;
    };
    let cutoff = now - Duration::days(days);

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        // Session files, plus temps orphaned by a crash between write and
        // rename — nothing will ever complete those.
        let ours =
            path.extension().and_then(|e| e.to_str()) == Some("json") || name.ends_with(".tmp");
        if !ours {
            continue;
        }

        let stale = |p: &Path| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .map(|modified| DateTime::<Utc>::from(modified) < cutoff)
                // Cannot tell how old it is, so leave it alone rather than risk
                // deleting a live session.
                .unwrap_or(false)
        };

        if !stale(&path) {
            continue;
        }

        // Between the check above and the unlink below, the owning session may
        // resume and rewrite this file — measured at about 1% under load, and
        // deleting a live session's state costs a spurious chime. Only stale
        // candidates pay for the lock, so the common case stays a bare `stat`.
        let Some(_guard) = lock_path(&path) else {
            continue;
        };
        if stale(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ------------------------------------------------------------------------ mute

/// The instant after which beckon may speak again, if muted.
pub fn read_mute(paths: &Paths) -> Option<DateTime<Utc>> {
    read_ts(&paths.mute_file)
}

pub fn write_mute(paths: &Paths, until: DateTime<Utc>) {
    write_ts(&paths.mute_file, until);
}

pub fn clear_mute(paths: &Paths) {
    let _ = std::fs::remove_file(&paths.mute_file);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::FileTimes;

    fn home() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::resolve_with(Some(d.path()));
        (d, p)
    }

    /// Backdate a session file's mtime, which is what pruning now looks at.
    fn backdate(paths: &Paths, session_id: &str, when: DateTime<Utc>) {
        let path = session_file(paths, session_id);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_times(FileTimes::new().set_modified(when.into()))
            .unwrap();
    }

    #[test]
    fn session_ids_cannot_escape_the_sessions_directory() {
        assert_eq!(sanitize_session_id("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_session_id("a/b\\c"), "abc");
        assert_eq!(sanitize_session_id("abc-123_XYZ"), "abc-123_XYZ");
        assert_eq!(sanitize_session_id(""), "unknown");
        assert_eq!(sanitize_session_id(".."), "unknown");
        assert_eq!(sanitize_session_id("~"), "unknown");
        assert_eq!(sanitize_session_id("a b"), "ab");
        assert_eq!(sanitize_session_id("a\0b"), "ab");
    }

    #[test]
    fn a_sanitized_id_never_contains_a_path_separator_or_dot() {
        for raw in ["../x", "..\\x", "/etc/passwd", "a/../b", "....", "%2e%2e"] {
            let s = sanitize_session_id(raw);
            assert!(!s.is_empty(), "{raw:?} sanitized to nothing");
            assert!(!s.contains('.'), "{raw:?} -> {s:?} kept a dot");
            assert!(!s.contains('/'), "{raw:?} -> {s:?} kept a slash");
            assert!(!s.contains('\\'), "{raw:?} -> {s:?} kept a backslash");
        }
    }

    #[test]
    fn absurdly_long_ids_are_truncated() {
        assert_eq!(sanitize_session_id(&"x".repeat(500)).len(), 128);
    }

    #[test]
    fn a_written_session_file_lands_inside_the_sessions_directory() {
        let (_d, p) = home();
        record_turn_start(&p, "../../escape", Utc::now());
        let entries: Vec<_> = std::fs::read_dir(&p.sessions_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].starts_with(&p.sessions_dir),
            "{:?} escaped",
            entries[0]
        );
    }

    #[test]
    fn turn_start_round_trips() {
        let (_d, p) = home();
        let now = Utc::now();
        assert_eq!(read_turn_start(&p, "sess-1"), None);
        record_turn_start(&p, "sess-1", now);
        assert_eq!(
            read_turn_start(&p, "sess-1").unwrap().timestamp(),
            now.timestamp()
        );
    }

    #[test]
    fn a_second_turn_overwrites_the_first() {
        let (_d, p) = home();
        let (t1, t2) = (Utc::now() - Duration::minutes(5), Utc::now());
        record_turn_start(&p, "s", t1);
        record_turn_start(&p, "s", t2);
        assert_eq!(
            read_turn_start(&p, "s").unwrap().timestamp(),
            t2.timestamp()
        );
    }

    #[test]
    fn sessions_are_isolated_from_each_other() {
        let (_d, p) = home();
        let t1 = Utc::now();
        let t2 = t1 + Duration::seconds(90);
        record_turn_start(&p, "a", t1);
        record_turn_start(&p, "b", t2);
        assert_eq!(
            read_turn_start(&p, "a").unwrap().timestamp(),
            t1.timestamp()
        );
        assert_eq!(
            read_turn_start(&p, "b").unwrap().timestamp(),
            t2.timestamp()
        );
    }

    #[test]
    fn last_played_is_keyed_by_session_and_state() {
        let (_d, p) = home();
        let now = Utc::now();
        record_played(&p, "a", State::Done, now);

        assert_eq!(
            read_last_played(&p, "a", State::Done).unwrap().timestamp(),
            now.timestamp()
        );
        assert_eq!(
            read_last_played(&p, "a", State::NeedsYou),
            None,
            "different state"
        );
        assert_eq!(
            read_last_played(&p, "b", State::Done),
            None,
            "different session"
        );
    }

    #[test]
    fn recording_a_sound_preserves_the_turn_start() {
        let (_d, p) = home();
        let started = Utc::now() - Duration::minutes(2);
        record_turn_start(&p, "s", started);
        record_played(&p, "s", State::Done, Utc::now());
        assert_eq!(
            read_turn_start(&p, "s").unwrap().timestamp(),
            started.timestamp()
        );
    }

    #[test]
    fn recording_a_turn_start_preserves_played_history() {
        let (_d, p) = home();
        let played = Utc::now() - Duration::minutes(2);
        record_played(&p, "s", State::Failed, played);
        record_turn_start(&p, "s", Utc::now());
        assert_eq!(
            read_last_played(&p, "s", State::Failed)
                .unwrap()
                .timestamp(),
            played.timestamp()
        );
    }

    #[test]
    fn several_states_coexist_in_one_session() {
        let (_d, p) = home();
        let now = Utc::now();
        for state in [State::Done, State::NeedsYou, State::Failed] {
            record_played(&p, "s", state, now);
        }
        for state in [State::Done, State::NeedsYou, State::Failed] {
            assert!(
                read_last_played(&p, "s", state).is_some(),
                "{state} missing"
            );
        }
    }

    #[test]
    fn a_session_that_only_played_still_reads_back() {
        // No UserPromptSubmit was ever seen, so there is no turn start.
        let (_d, p) = home();
        record_played(&p, "s", State::Done, Utc::now());
        assert_eq!(read_turn_start(&p, "s"), None);
        assert!(read_last_played(&p, "s", State::Done).is_some());
    }

    #[test]
    fn prune_session_removes_only_that_session() {
        let (_d, p) = home();
        record_turn_start(&p, "a", Utc::now());
        record_turn_start(&p, "b", Utc::now());
        prune_session(&p, "a");
        assert!(read_turn_start(&p, "a").is_none());
        assert!(read_turn_start(&p, "b").is_some());
    }

    #[test]
    fn prune_older_than_removes_sessions_untouched_for_too_long() {
        let (_d, p) = home();
        let now = Utc::now();
        record_turn_start(&p, "old", now);
        record_turn_start(&p, "fresh", now);
        backdate(&p, "old", now - Duration::days(30));

        prune_older_than(&p, now, 7);
        assert!(read_turn_start(&p, "old").is_none());
        assert!(read_turn_start(&p, "fresh").is_some());
    }

    #[test]
    fn an_active_session_is_never_pruned_however_old_its_first_turn() {
        // mtime tracks last activity, not when the session began.
        let (_d, p) = home();
        let now = Utc::now();
        record_turn_start(&p, "s", now - Duration::days(60));
        prune_older_than(&p, now, 7);
        assert!(read_turn_start(&p, "s").is_some());
    }

    #[test]
    fn prune_collects_temp_files_orphaned_by_a_crash() {
        let (_d, p) = home();
        let now = Utc::now();
        std::fs::create_dir_all(&p.sessions_dir).unwrap();
        let orphan = p.sessions_dir.join(".sessX.json.4242.tmp");
        std::fs::write(&orphan, b"partial").unwrap();
        let file = std::fs::File::options().write(true).open(&orphan).unwrap();
        file.set_times(FileTimes::new().set_modified((now - Duration::days(30)).into()))
            .unwrap();

        prune_older_than(&p, now, 7);
        assert!(!orphan.exists(), "orphaned temp file was never collected");
    }

    #[test]
    fn prune_leaves_unrelated_files_alone() {
        let (_d, p) = home();
        let now = Utc::now();
        std::fs::create_dir_all(&p.sessions_dir).unwrap();
        let foreign = p.sessions_dir.join("README");
        std::fs::write(&foreign, b"not ours").unwrap();
        let file = std::fs::File::options().write(true).open(&foreign).unwrap();
        file.set_times(FileTimes::new().set_modified((now - Duration::days(90)).into()))
            .unwrap();

        prune_older_than(&p, now, 7);
        assert!(
            foreign.exists(),
            "pruning must not touch files it does not own"
        );
    }

    #[test]
    fn prune_on_a_missing_directory_is_a_no_op() {
        let (_d, p) = home();
        prune_older_than(&p, Utc::now(), 7);
        prune_session(&p, "nope");
    }

    #[test]
    fn corrupt_session_file_reads_as_empty_not_an_error() {
        let (_d, p) = home();
        std::fs::create_dir_all(&p.sessions_dir).unwrap();
        std::fs::write(p.sessions_dir.join("bad.json"), b"\x00garbage").unwrap();
        assert_eq!(read_turn_start(&p, "bad"), None);
        assert_eq!(read_last_played(&p, "bad", State::Done), None);
    }

    #[test]
    fn a_session_file_from_a_future_version_does_not_break_reads() {
        // Forward compatibility: unknown fields must not discard known ones.
        let (_d, p) = home();
        std::fs::create_dir_all(&p.sessions_dir).unwrap();
        let now = Utc::now();
        std::fs::write(
            p.sessions_dir.join("v2.json"),
            format!(
                r#"{{"turn_started":"{}","something_new":42}}"#,
                now.to_rfc3339()
            ),
        )
        .unwrap();
        assert_eq!(
            read_turn_start(&p, "v2").unwrap().timestamp(),
            now.timestamp()
        );
    }

    #[test]
    fn player_slots_are_capped() {
        // Nothing bounded this before: 240 rapid events produced 241 players.
        let (_d, p) = home();
        let held: Vec<_> = (0..MAX_CONCURRENT_PLAYERS)
            .map(|_| claim_player_slot(&p).unwrap())
            .collect();
        assert_eq!(held.len(), MAX_CONCURRENT_PLAYERS);
        assert!(claim_player_slot(&p).is_none(), "the cap was not enforced");
    }

    #[test]
    fn a_slot_is_released_when_its_guard_drops() {
        let (_d, p) = home();
        {
            let _held: Vec<_> = (0..MAX_CONCURRENT_PLAYERS)
                .map(|_| claim_player_slot(&p).unwrap())
                .collect();
            assert!(claim_player_slot(&p).is_none());
        }
        assert!(
            claim_player_slot(&p).is_some(),
            "slots leaked after the guards dropped"
        );
    }

    #[test]
    fn a_slot_abandoned_by_a_dead_process_is_reclaimed() {
        let (_d, p) = home();
        let dir = p.state_dir.join("players");
        std::fs::create_dir_all(&dir).unwrap();

        // Every slot held, all of them long abandoned.
        for slot in 0..MAX_CONCURRENT_PLAYERS {
            let path = dir.join(format!("slot-{slot}"));
            std::fs::write(&path, b"").unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_times(
                FileTimes::new().set_modified((Utc::now() - Duration::minutes(10)).into()),
            )
            .unwrap();
        }
        assert!(
            claim_player_slot(&p).is_some(),
            "a dead process wedged playback forever"
        );
    }

    #[test]
    fn prune_leaves_a_locked_session_alone() {
        // The stat-then-unlink race: the owning session resumes between the
        // check and the removal. Only stale candidates take the lock, so this
        // costs nothing in the common case.
        let (_d, p) = home();
        let now = Utc::now();
        record_turn_start(&p, "live", now);
        backdate(&p, "live", now - Duration::days(30));

        let held = lock_path(&session_file(&p, "live")).expect("lock should be free");
        prune_older_than(&p, now, 7);
        assert!(
            read_turn_start(&p, "live").is_some(),
            "pruned a session under lock"
        );

        drop(held);
        prune_older_than(&p, now, 7);
        assert!(
            read_turn_start(&p, "live").is_none(),
            "stale session should now be collected"
        );
    }

    #[test]
    fn mute_round_trips_and_clears() {
        let (_d, p) = home();
        assert_eq!(read_mute(&p), None);
        let until = Utc::now() + Duration::minutes(30);
        write_mute(&p, until);
        assert_eq!(read_mute(&p).unwrap().timestamp(), until.timestamp());
        clear_mute(&p);
        assert_eq!(read_mute(&p), None);
    }

    #[test]
    fn clearing_an_unset_mute_is_a_no_op() {
        let (_d, p) = home();
        clear_mute(&p);
        assert_eq!(read_mute(&p), None);
    }

    #[test]
    fn an_unwritable_home_degrades_silently_instead_of_panicking() {
        let p = Paths::resolve_with(Some(Path::new("/proc/nonexistent/beckon")));
        record_turn_start(&p, "s", Utc::now());
        record_played(&p, "s", State::Done, Utc::now());
        write_mute(&p, Utc::now());
        prune_older_than(&p, Utc::now(), 7);
        assert_eq!(read_turn_start(&p, "s"), None);
        assert_eq!(read_last_played(&p, "s", State::Done), None);
    }

    #[test]
    fn no_temp_files_are_left_behind() {
        let (_d, p) = home();
        record_turn_start(&p, "a", Utc::now());
        record_played(&p, "a", State::Done, Utc::now());
        let strays: Vec<_> = std::fs::read_dir(&p.sessions_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "left temp files: {strays:?}");
    }
}
