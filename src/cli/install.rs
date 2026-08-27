//! `beckon init` and `beckon uninstall`.
//!
//! These edit a file that can make the agent run arbitrary commands. Every
//! guarantee here exists because of that:
//!
//! - the exact diff is shown before anything is written;
//! - the original is copied beside itself first, under an obvious name;
//! - `uninstall` removes exactly beckon's own entries and nothing else;
//! - a file that will not parse aborts the whole operation rather than being
//!   overwritten with something "clean".

use crate::adapter::{adapter_for, Adapter, Scope};
use crate::core::paths;
use crate::settings_json::{self, InstallPlan};
use serde_json::Value;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub struct Options {
    pub agent: String,
    pub scope: Scope,
    pub dry_run: bool,
    pub assume_yes: bool,
}

pub fn init(options: Options) -> i32 {
    let Some((adapter, settings_path)) = resolve(&options) else {
        return 1;
    };

    let existing = match read_settings(&settings_path) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };

    let command = match invocation() {
        Some(command) => command,
        None => {
            eprintln!("cannot determine beckon's own path, so hooks cannot be written");
            return 1;
        }
    };

    let plan: InstallPlan = adapter.install_plan(&command);
    let merged = match settings_json::merge(&existing, &plan) {
        Ok(merged) => merged,
        Err(e) => {
            eprintln!(
                "{} has a shape beckon does not recognise: {e}.\n\
                 Refusing to guess. Fix or move the file, then retry.",
                settings_path.display()
            );
            return 1;
        }
    };

    if merged == existing {
        println!(
            "Already installed for {} ({} scope).",
            options.agent,
            options.scope.as_str()
        );
        println!("  {}", settings_path.display());
        println!("\nRun `beckon doctor` to check it over, or `beckon test` to hear the pack.");
        return 0;
    }

    println!(
        "beckon will bind {} hooks for {}:",
        plan.bindings.len(),
        options.agent
    );
    println!("  file    {}", settings_path.display());
    println!("  command {command}");
    println!();
    print_diff(&existing, &merged);

    apply(
        &settings_path,
        &existing,
        &merged,
        &options,
        "installed",
        &[
            ("beckon doctor", "confirm what beckon will do"),
            ("beckon test", "hear the active pack"),
        ],
    )
}

pub fn uninstall(options: Options) -> i32 {
    let Some((_, settings_path)) = resolve(&options) else {
        return 1;
    };

    if !settings_path.exists() {
        println!(
            "Nothing to remove — {} does not exist.",
            settings_path.display()
        );
        return 0;
    }

    let existing = match read_settings(&settings_path) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };

    let restored = match settings_json::unmerge(&existing) {
        Ok(restored) => restored,
        Err(e) => {
            eprintln!(
                "{} has a shape beckon does not recognise: {e}.\n\
                 Refusing to guess. Fix or move the file, then retry.",
                settings_path.display()
            );
            return 1;
        }
    };
    if restored == existing {
        println!("No beckon hooks found in {}.", settings_path.display());
        return 0;
    }

    println!(
        "beckon will remove its hooks from {}:",
        settings_path.display()
    );
    println!();
    print_diff(&existing, &restored);

    apply(
        &settings_path,
        &existing,
        &restored,
        &options,
        "removed",
        &[("beckon init", "bind them again whenever you want")],
    )
}

/// Resolve the adapter and the file it owns, reporting clearly on failure.
fn resolve(options: &Options) -> Option<(Box<dyn Adapter>, PathBuf)> {
    let Some(adapter) = adapter_for(&options.agent) else {
        eprintln!("unknown agent `{}`. Supported: claude-code", options.agent);
        return None;
    };

    let cwd = std::env::current_dir().unwrap_or_default();
    let root = match options.scope {
        Scope::User => cwd.clone(),
        // Project scope must mean *this repository*, not "wherever a marker
        // happens to sit". Without this, a dotfiles repo at $HOME makes
        // `--scope project` write the user-scope file instead.
        Scope::Project => match paths::vcs_root(&cwd) {
            Some(root) => root,
            None => {
                eprintln!(
                    "not inside a repository, so there is no project to install into.\n\
                     Run this from a repository, or use `--scope user`."
                );
                return None;
            }
        },
    };

    let Some(path) = adapter.settings_path(options.scope, &root) else {
        eprintln!("cannot locate {}'s settings file", options.agent);
        return None;
    };
    Some((adapter, path))
}

/// The absolute invocation to write into the hook.
///
/// Absolute rather than bare `beckon`: hooks can run with a minimal `PATH`, and
/// a hook that silently fails to resolve is indistinguishable from a broken one.
fn invocation() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe = simplify(exe.canonicalize().unwrap_or(exe));
    let path = exe.display().to_string();
    Some(if path.contains(char::is_whitespace) {
        format!("\"{path}\" hook claude-code")
    } else {
        format!("{path} hook claude-code")
    })
}

/// Undo the extended-length prefix `canonicalize` adds on Windows.
///
/// `canonicalize` returns `\\?\D:\path\beckon.exe`. That form is valid for
/// `CreateProcess` but many shells and launchers cannot run it, and this string
/// is handed to the agent to execute — so a hook written on Windows could
/// simply never fire. Ordinary drive paths are stripped back to `D:\path`;
/// genuine UNC paths (`\\?\UNC\server\share`) are left alone, since they have
/// no shorter form.
#[cfg(windows)]
fn simplify(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// Nothing to undo: `canonicalize` returns a plain absolute path on unix.
#[cfg(not(windows))]
fn simplify(path: PathBuf) -> PathBuf {
    path
}

/// Read and parse.
///
/// A missing file is an empty document; **anything else that cannot be read
/// faithfully is fatal**. An earlier version treated an unreadable file as
/// empty, so a settings.json saved as UTF-16 — which is what Windows Notepad
/// and PowerShell produce — was silently replaced with beckon's hooks alone,
/// and the preview cheerfully showed it as having been empty.
fn read_settings(path: &Path) -> Result<Value, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Default::default()))
        }
        Err(e) => return Err(format!("{}: cannot be read ({e}).", path.display())),
    };

    // A FIFO would block forever; a directory or device is not a settings file.
    if !metadata.is_file() && !metadata.file_type().is_symlink() {
        return Err(format!(
            "{} is not a regular file. Refusing to touch it.",
            path.display()
        ));
    }

    let bytes =
        std::fs::read(path).map_err(|e| format!("{}: cannot be read ({e}).", path.display()))?;
    if bytes.is_empty() {
        return Ok(Value::Object(Default::default()));
    }

    let text = String::from_utf8(bytes).map_err(|_| {
        format!(
            "{} is not valid UTF-8 — it may be UTF-16, as saved by some Windows \
             editors.\nRefusing to touch it. Re-save it as UTF-8 and try again.",
            path.display()
        )
    })?;
    if text.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }

    serde_json::from_str(&text).map_err(|e| {
        format!(
            "{} is not valid JSON ({e}).\nRefusing to touch it. Fix or move the file, \
             then retry.",
            path.display()
        )
    })
}

fn print_diff(before: &Value, after: &Value) {
    for line in settings_json::diff(before, after).lines() {
        println!("  {line}");
    }
    println!();

    let untouched = settings_json::foreign_hooks(after);
    if untouched.is_empty() {
        println!("  No other hooks in this file.");
    } else {
        println!("  Left untouched — {} other hook(s):", untouched.len());
        for (event, command) in &untouched {
            println!(
                "    {}: {}",
                crate::cli::safe(event),
                crate::cli::safe(command)
            );
        }
    }
    println!();
}

fn apply(
    path: &Path,
    before: &Value,
    after: &Value,
    options: &Options,
    verb: &str,
    next_steps: &[(&str, &str)],
) -> i32 {
    if options.dry_run {
        println!("Dry run — nothing written.");
        return 0;
    }
    if !confirm(options.assume_yes) {
        println!("Cancelled. Nothing written.");
        return 1;
    }

    // The prompt is a window during which the agent itself may rewrite this
    // file — approving a permission does exactly that. Committing the document
    // we read beforehand would silently discard whatever landed in between.
    match read_settings(path) {
        Ok(current) if &current != before => {
            eprintln!(
                "{} changed while you were deciding. Nothing written — \
                 re-run to see the current diff.",
                path.display()
            );
            return 1;
        }
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
        _ => {}
    }

    if path.exists() {
        match backup(path) {
            Ok(Some(copy)) => println!("Backed up to {}", copy.display()),
            Ok(None) => {}
            Err(e) => {
                eprintln!("could not back up {}: {e}\nAborting.", path.display());
                return 1;
            }
        }
    }

    if let Err(e) = write_settings(path, after) {
        eprintln!("could not write {}: {e}", path.display());
        return 1;
    }

    let changed = settings_json::diff(before, after)
        .lines()
        .filter(|l| l.starts_with('+') || l.starts_with('-'))
        .count();
    println!("Hooks {verb} ({changed} lines changed).");
    if !next_steps.is_empty() {
        println!();
        println!("Next:");
        for (command, why) in next_steps {
            println!("  {command:<16} {why}");
        }
    }
    0
}

fn confirm(assume_yes: bool) -> bool {
    if assume_yes {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("Not a terminal — re-run with --yes to confirm, or --dry-run to preview.");
        return false;
    }
    eprint!("Apply these changes? [y/N] ");
    use std::io::Write;
    let _ = std::io::stderr().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Copy beside the original, under a name that explains itself.
///
/// Never overwrites an existing backup. An init followed by an uninstall lands
/// in the same second, and clobbering the first copy would destroy the only
/// record of what the file looked like before beckon ever touched it.
fn backup(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    for attempt in 0..100 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let copy = path.with_file_name(format!("{base}.beckon-backup-{stamp}{suffix}"));
        if !copy.exists() {
            std::fs::copy(path, &copy)?;
            return Ok(Some(copy));
        }
    }
    Err(std::io::Error::other(
        "could not find an unused backup name",
    ))
}

fn write_settings(path: &Path, value: &Value) -> std::io::Result<()> {
    // Write through a symlink to its target. Renaming over the link itself
    // would replace it with a regular file, quietly severing the dotfiles
    // repository someone is managing this file with.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Ok(metadata) = std::fs::metadata(&target) {
        if metadata.permissions().readonly() {
            return Err(std::io::Error::other(format!(
                "{} is read-only — beckon will not override that",
                target.display()
            )));
        }
    }

    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');

    // Temp plus rename, so an interrupted write cannot leave the agent with a
    // truncated settings file. The temp is removed if anything goes wrong, so a
    // full disk does not litter the directory.
    let tmp = target.with_extension(format!("beckon-tmp-{}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, text) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn an_extended_length_prefix_is_stripped() {
        // This string is executed by the agent. `canonicalize` produces the
        // `\\?\` form, which many launchers cannot run — a hook written on
        // Windows would then never fire.
        assert_eq!(
            simplify(PathBuf::from(r"\\?\D:\tools\beckon.exe")),
            PathBuf::from(r"D:\tools\beckon.exe")
        );
    }

    #[test]
    fn a_genuine_unc_path_is_left_alone() {
        // `\\?\UNC\server\share` has no shorter equivalent.
        let unc = PathBuf::from(r"\\?\UNC\server\share\beckon.exe");
        assert_eq!(simplify(unc.clone()), unc);
    }

    #[test]
    fn an_ordinary_path_is_unchanged() {
        let plain = PathBuf::from(r"D:\tools\beckon.exe");
        assert_eq!(simplify(plain.clone()), plain);
    }

    #[test]
    fn the_invocation_never_carries_an_extended_prefix() {
        let command = invocation().expect("current_exe is available under test");
        assert!(!command.contains(r"\\?\"), "unrunnable command: {command}");
        assert!(command.ends_with("hook claude-code"), "{command}");
    }
}
