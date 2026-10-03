//! Reading and writing files that someone else may have arranged.
//!
//! beckon runs inside the agent's hooks with the agent's working directory, so
//! the files it touches are not all the user's own: a cloned repository chooses
//! what `.beckon.toml` is, a downloaded pack chooses what `pack.toml` is, and on
//! a shared machine another account can plant things in `/tmp`. Every read and
//! write that can meet such a file goes through here, so the rules hold
//! everywhere rather than wherever someone remembered them:
//!
//! - **Reads are of regular files only, and bounded.** A symlink to
//!   `/dev/zero` (which git will happily commit) used to read until memory ran
//!   out; a FIFO blocked the hook — and with it the agent — forever.
//! - **Writes are exclusive and private.** A replacement is created fresh with
//!   `create_new`, so it can never follow a planted symlink, and with the mode
//!   decided up front, so a 0600 file never passes through 0664 on its way.

use std::io::{Read, Write};
use std::path::Path;

/// Size limits for the files beckon reads. Each is far above anything real and
/// far below anything that hurts.
pub mod limit {
    /// `config.toml` and `.beckon.toml`. Real ones are a few hundred bytes.
    pub const CONFIG: u64 = 64 * 1024;
    /// A pack manifest. The built-in ones are about 2 KB.
    pub const PACK_MANIFEST: u64 = 256 * 1024;
    /// An agent's settings file, which can legitimately grow large.
    pub const SETTINGS: u64 = 8 * 1024 * 1024;
    /// beckon's own small state files.
    pub const STATE: u64 = 64 * 1024;
}

/// Read a regular file as UTF-8, refusing anything else and anything over
/// `max` bytes. Errors read as sentences, because they end up in `doctor`.
pub fn read_bounded(path: &Path, max: u64) -> std::io::Result<String> {
    // Follows symlinks deliberately: a dotfiles link to a real config is fine.
    // What it points at must be a regular file — not a device, not a FIFO.
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path)?;
    // Checked again on the open handle: the path could have been swapped for
    // a device between the two calls.
    if !file.metadata()?.is_file() {
        return Err(not_regular());
    }

    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(std::io::Error::other(format!(
            "larger than {} KiB, so not read",
            max / 1024
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not valid UTF-8"))
}

fn not_regular() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "not a regular file, so not read",
    )
}

/// Is `path` itself a symlink? (Not what it points to.)
pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Create `path` exclusively, with the same permissions as `like` — or owner
/// read/write only when `like` does not exist.
///
/// Exclusive, so it can never follow a planted symlink or clobber a file. The
/// mode is also set explicitly after creation: the umask can only remove bits,
/// and the original may have had more.
pub fn create_private_like(path: &Path, like: &Path) -> std::io::Result<std::fs::File> {
    create_exclusive(path, mode_of(like).unwrap_or(PRIVATE))
}

/// Owner read/write only.
const PRIVATE: u32 = 0o600;

#[cfg(unix)]
fn mode_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

#[cfg(unix)]
fn create_exclusive(path: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    Ok(file)
}

/// Windows has no mode bits; a new file inherits its directory's ACL, which
/// for everything beckon writes is inside the user's own profile.
#[cfg(not(unix))]
fn mode_of(_path: &Path) -> Option<u32> {
    None
}

#[cfg(not(unix))]
fn create_exclusive(path: &Path, _mode: u32) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Replace `path` with `bytes`, atomically and keeping its permissions — 0600
/// for a file that does not exist yet. For files the user owns and may have
/// chosen a mode for: settings, config.
///
/// Through a symlink to its target, so a dotfiles link survives rather than
/// being replaced by a regular file. Via an exclusive temp file with the mode
/// already set, then a rename, so a reader never sees half a file and the
/// result is never briefly more readable than before.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    replace_with(path, bytes, None, true)
}

/// [`replace`], but always 0600 — for beckon's own state, which nobody else
/// has a reason to read, including files an older version left 0664.
///
/// Not synced to disk: this runs inside hooks the agent waits on, and losing
/// state to a power cut costs one extra chime, not a setting.
pub fn replace_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    replace_with(path, bytes, Some(PRIVATE), false)
}

fn replace_with(
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    durable: bool,
) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let Some(parent) = target.parent() else {
        return Err(std::io::Error::other("no parent directory"));
    };
    std::fs::create_dir_all(parent)?;

    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // `.tmp`, which state pruning collects if a crash strands one.
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    // A leftover from a crash with a recycled pid; ours to remove.
    let _ = std::fs::remove_file(&tmp);

    let mode = mode.or_else(|| mode_of(&target)).unwrap_or(PRIVATE);
    let written = create_exclusive(&tmp, mode).and_then(|mut file| {
        file.write_all(bytes)?;
        if durable {
            file.sync_all()?;
        }
        Ok(())
    });
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, &target)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Open a diagnostics file for appending: private, and never through a link.
///
/// For `BECKON_TRACE` and `BECKON_DUMP`, which `doctor` used to suggest putting
/// in `/tmp`. The dump holds whole prompts, so it must not be readable by
/// other accounts; and a path in a shared directory may already be a symlink
/// someone else planted, pointing at a file of yours they would like appended
/// to. An existing file must be a regular file, not a link to one.
pub fn open_private_append(path: &Path) -> Option<std::fs::File> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if !m.file_type().is_file() => return None,
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }

    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW closes the gap between the check above and this open: a
        // link swapped in meanwhile makes the open fail instead of following.
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).ok()?;
    file.metadata().ok().filter(|m| m.is_file())?;
    // An existing file keeps whatever mode it had, so make it private on the
    // handle — and if that is not allowed, it belongs to someone else, who
    // would then be reading every prompt written to it. Refuse.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .ok()?;
    }
    Some(file)
}

/// Is `dir` writable by every account on the machine?
///
/// Such a directory — `/tmp`, `/var/tmp`, `/dev/shm` — is somewhere anyone can
/// plant a file, so nothing found there speaks for the user. The sticky bit
/// does not change that: it stops others deleting your files, not creating
/// their own.
#[cfg(unix)]
pub fn world_writable(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir).is_ok_and(|m| m.permissions().mode() & 0o002 != 0)
}

/// Windows ACLs do not reduce to one bit. The shared-directory concern there
/// is narrower and handled by stopping at the user's home directory.
#[cfg(not(unix))]
pub fn world_writable(_dir: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_regular_file_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.toml");
        std::fs::write(&path, "volume = 0.5\n").unwrap();
        assert_eq!(read_bounded(&path, 1024).unwrap(), "volume = 0.5\n");
    }

    #[test]
    fn a_file_over_the_limit_is_refused_without_reading_it_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.toml");
        std::fs::write(&path, vec![b'#'; 4096]).unwrap();
        let err = read_bounded(&path, 1024).unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
        assert_eq!(read_bounded(&path, 4096).unwrap().len(), 4096);
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_bounded(dir.path(), 1024).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_device_is_refused_rather_than_read_forever() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".beckon.toml");
        std::os::unix::fs::symlink("/dev/zero", &path).unwrap();
        let err = read_bounded(&path, 1024).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_rather_than_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".beckon.toml");
        let status = std::process::Command::new("mkfifo").arg(&path).status();
        if !status.is_ok_and(|s| s.success()) {
            return; // no mkfifo here; the device case covers the same check
        }
        assert!(read_bounded(&path, 1024).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replace_keeps_a_private_file_private_and_a_symlink_a_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.toml");
        std::fs::write(&real, "a").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        replace(&link, b"b").unwrap();
        assert!(is_symlink(&link), "the link was replaced by a file");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "b");
        let mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn replace_creates_a_new_file_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new");
        replace(&path, b"x").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "temp file left behind");
    }

    #[cfg(unix)]
    #[test]
    fn replace_private_tightens_a_loose_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
        replace_private(&path, b"{}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn diagnostics_are_private_and_never_written_through_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("trace.log");
        let mut file = open_private_append(&log).unwrap();
        file.write_all(b"x\n").unwrap();
        let mode = std::fs::metadata(&log).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let victim = dir.path().join("bashrc");
        std::fs::write(&victim, "").unwrap();
        let planted = dir.path().join("planted.log");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        assert!(open_private_append(&planted).is_none());
        // An existing loose file is tightened rather than written to as-is.
        let loose = dir.path().join("loose.log");
        std::fs::write(&loose, "").unwrap();
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(open_private_append(&loose).is_some());
        let mode = std::fs::metadata(&loose).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let dangling = dir.path().join("dangling.log");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();
        assert!(open_private_append(&dangling).is_none());
        assert!(!dir.path().join("nowhere").exists());
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_recognises_tmp_like_directories() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!world_writable(dir.path()));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(world_writable(dir.path()));
    }
}
