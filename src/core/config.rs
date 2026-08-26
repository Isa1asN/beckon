//! Layered configuration.
//!
//! Later layers win: built-in defaults, then the user's `config.toml`, then the
//! project's `.beckon.toml`, then `BECKON_*` environment variables.
//!
//! Loading **never fails**. A malformed layer is reported as a warning and
//! skipped. Callers decide what to do with warnings: CLI commands print them,
//! the hook path traces them, because stderr noise during a hook is noise in
//! someone's agent session.

use crate::core::event::State;
use crate::core::paths::Paths;
use chrono::NaiveTime;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- public types

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub pack: String,
    pub volume: f32,
    pub enabled: bool,
    /// Per-state overrides pointing at the user's own audio files.
    ///
    /// Accepted **only** from the user's own config. See [`Config::load_layers`].
    pub sounds: BTreeMap<State, PathBuf>,
    pub policy: PolicyConfig,
    pub events: EventsConfig,
    pub identity: IdentityConfig,
    pub remote: RemoteConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyConfig {
    /// `done` stays silent for turns shorter than this — a short turn means you
    /// were still watching.
    pub min_turn_seconds: u64,
    /// Minimum gap between any two sounds. One sound is enough to make you look.
    pub rate_limit_ms: u64,
    /// States exempt from the duration gate. Still subject to the rate limit.
    pub always_alert: Vec<State>,
    pub quiet_hours: Option<QuietHours>,
    pub quiet_hours_action: QuietAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuietHours {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QuietAction {
    Silence,
    Volume(f32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventsConfig(BTreeMap<State, bool>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityConfig {
    /// Give each project a stable transposition so parallel agents differ.
    pub per_project: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    pub mode: RemoteMode,
    pub sequences: Vec<Sequence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteMode {
    /// Escape sequences only when SSH is detected.
    Auto,
    Off,
    /// Sequences regardless of SSH, and no local audio.
    Always,
    /// Local audio *and* sequences.
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sequence {
    Bel,
    Osc9,
    Osc777,
}

// ------------------------------------------------------------------- behaviour

impl Default for Config {
    fn default() -> Self {
        Config {
            pack: "aurora".into(),
            volume: 0.6,
            enabled: true,
            sounds: BTreeMap::new(),
            policy: PolicyConfig {
                min_turn_seconds: 30,
                rate_limit_ms: 1500,
                always_alert: vec![
                    State::NeedsYou,
                    State::RateLimited,
                    State::IdleWaiting,
                    State::Failed,
                ],
                quiet_hours: None,
                quiet_hours_action: QuietAction::Silence,
            },
            events: EventsConfig::defaults(),
            identity: IdentityConfig { per_project: true },
            remote: RemoteConfig {
                mode: RemoteMode::Auto,
                sequences: vec![Sequence::Bel, Sequence::Osc9],
            },
        }
    }
}

impl EventsConfig {
    pub fn defaults() -> Self {
        let mut m = BTreeMap::new();
        for s in State::ALL {
            // tool-failed stays off on purpose: a failing test is normal work,
            // and chiming on every non-zero exit is how you get uninstalled.
            let on = matches!(
                s,
                State::Done
                    | State::NeedsYou
                    | State::Failed
                    | State::RateLimited
                    | State::IdleWaiting
            );
            m.insert(s, on);
        }
        EventsConfig(m)
    }

    pub fn enabled(&self, s: State) -> bool {
        self.0.get(&s).copied().unwrap_or(false)
    }

    pub fn set(&mut self, s: State, on: bool) {
        self.0.insert(s, on);
    }
}

impl QuietHours {
    /// True inside the window. Start is inclusive, end exclusive, and a window
    /// whose end precedes its start wraps midnight — which is the common case.
    pub fn contains(&self, t: NaiveTime) -> bool {
        if self.start <= self.end {
            t >= self.start && t < self.end
        } else {
            t >= self.start || t < self.end
        }
    }
}

impl std::str::FromStr for QuietHours {
    type Err = String;

    /// `"23:00-08:00"`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (a, b) = s
            .split_once('-')
            .ok_or_else(|| format!("expected `HH:MM-HH:MM`, got {s:?}"))?;
        let parse = |part: &str| {
            NaiveTime::parse_from_str(part.trim(), "%H:%M")
                .map_err(|_| format!("expected `HH:MM`, got {part:?}"))
        };
        let (start, end) = (parse(a)?, parse(b)?);
        if start == end {
            // `contains` treats this as an empty window, i.e. never quiet —
            // almost certainly the opposite of what was meant.
            return Err(format!(
                "a window cannot start and end at {}; for all day use `00:00-23:59`",
                start.format("%H:%M")
            ));
        }
        Ok(QuietHours { start, end })
    }
}

impl std::str::FromStr for QuietAction {
    type Err = String;

    /// `"silence"` or `"volume:0.2"`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "silence" => Ok(QuietAction::Silence),
            other => match other.strip_prefix("volume:") {
                Some(v) => v
                    .trim()
                    .parse::<f32>()
                    .map(|v| QuietAction::Volume(v.clamp(0.0, 1.0)))
                    .map_err(|_| format!("expected a number after `volume:`, got {v:?}")),
                None => Err(format!("expected `silence` or `volume:<0..1>`, got {s:?}")),
            },
        }
    }
}

/// A config plus anything questionable found while loading it.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
}

impl Config {
    /// Load and discard warnings. Use [`Config::load_verbose`] to surface them.
    pub fn load(paths: &Paths, project_root: Option<&Path>) -> Config {
        Config::load_verbose(paths, project_root).config
    }

    pub fn load_verbose(paths: &Paths, project_root: Option<&Path>) -> Loaded {
        Config::load_layers(paths, project_root, |k| std::env::var(k).ok())
    }

    /// The real entry point, with the environment injected so tests never have
    /// to mutate process state.
    pub fn load_layers<F>(paths: &Paths, project_root: Option<&Path>, get_env: F) -> Loaded
    where
        F: Fn(&str) -> Option<String>,
    {
        let mut warnings = Vec::new();
        let mut merged = PartialConfig::default();

        let mut layers = vec![read_layer(&paths.config_file)];
        if let Some(root) = project_root {
            let mut project = read_layer(&root.join(PROJECT_CONFIG_FILE));
            // A project config arrives with the repository, so it is not the
            // user's word. Letting it name files would hand any repository you
            // clone the ability to make your machine open arbitrary paths and
            // feed them to a media decoder. Everything else it may set only
            // changes when beckon makes a noise.
            if !project.partial.sounds.is_empty() {
                let names: Vec<&str> = project.partial.sounds.keys().map(String::as_str).collect();
                project.warnings.push(format!(
                    "{}: ignoring [sounds] ({}) — sound files may only be set in your own \
                     config, not by a project",
                    root.join(PROJECT_CONFIG_FILE).display(),
                    names.join(", ")
                ));
                project.partial.sounds.clear();
            }
            layers.push(project);
        }
        layers.push(env_layer(get_env));

        for layer in layers {
            warnings.extend(layer.warnings);
            merged.merge(layer.partial);
        }

        let (config, more) = merged.materialize();
        warnings.extend(more);
        Loaded { config, warnings }
    }
}

/// Per-project override, read from the agent's working directory.
pub const PROJECT_CONFIG_FILE: &str = ".beckon.toml";

// ---------------------------------------------------------------- layer typing

/// Every field optional, so an unmentioned key inherits rather than resets.
///
/// Deliberately *not* `deny_unknown_fields`. Serde rejects the whole document on
/// one bad key, and this layer is skipped when it fails to parse — so a single
/// misplaced key used to discard `enabled = false` and make beckon speak when
/// the user had asked for silence. Unknown keys are reported by
/// [`unknown_keys`] instead, which warns without throwing away what parsed.
#[derive(Debug, Default, Deserialize)]
struct PartialConfig {
    pack: Option<String>,
    volume: Option<f32>,
    enabled: Option<bool>,
    #[serde(default)]
    policy: PartialPolicy,
    /// Kept as strings so an unknown state key warns instead of killing the layer.
    #[serde(default)]
    events: BTreeMap<String, bool>,
    /// State name to audio file path.
    #[serde(default)]
    sounds: BTreeMap<String, String>,
    #[serde(default)]
    identity: PartialIdentity,
    #[serde(default)]
    remote: PartialRemote,
}

#[derive(Debug, Default, Deserialize)]
struct PartialPolicy {
    min_turn_seconds: Option<u64>,
    rate_limit_ms: Option<u64>,
    /// Kept as strings so an unknown state warns instead of killing the layer.
    always_alert: Option<Vec<String>>,
    quiet_hours: Option<String>,
    quiet_hours_action: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PartialIdentity {
    per_project: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct PartialRemote {
    mode: Option<RemoteMode>,
    sequences: Option<Vec<Sequence>>,
}

struct Layer {
    partial: PartialConfig,
    warnings: Vec<String>,
}

impl PartialConfig {
    /// Later layers win, field by field. An unmentioned field inherits.
    fn merge(&mut self, other: PartialConfig) {
        fn take<T>(dst: &mut Option<T>, src: Option<T>) {
            if src.is_some() {
                *dst = src;
            }
        }

        take(&mut self.pack, other.pack);
        take(&mut self.volume, other.volume);
        take(&mut self.enabled, other.enabled);

        take(
            &mut self.policy.min_turn_seconds,
            other.policy.min_turn_seconds,
        );
        take(&mut self.policy.rate_limit_ms, other.policy.rate_limit_ms);
        take(&mut self.policy.always_alert, other.policy.always_alert);
        take(&mut self.policy.quiet_hours, other.policy.quiet_hours);
        take(
            &mut self.policy.quiet_hours_action,
            other.policy.quiet_hours_action,
        );

        take(&mut self.identity.per_project, other.identity.per_project);

        take(&mut self.remote.mode, other.remote.mode);
        take(&mut self.remote.sequences, other.remote.sequences);

        // Events merge key-by-key rather than replacing wholesale, so a project
        // can flip one event without restating the rest.
        self.events.extend(other.events);
        self.sounds.extend(other.sounds);
    }

    fn materialize(self) -> (Config, Vec<String>) {
        let mut warnings = Vec::new();
        let mut c = Config::default();

        if let Some(v) = self.pack {
            c.pack = v;
        }
        if let Some(v) = self.volume {
            c.volume = v.clamp(0.0, 1.0);
        }
        if let Some(v) = self.enabled {
            c.enabled = v;
        }
        if let Some(v) = self.policy.min_turn_seconds {
            c.policy.min_turn_seconds = v;
        }
        if let Some(v) = self.policy.rate_limit_ms {
            c.policy.rate_limit_ms = v;
        }
        if let Some(raw) = self.policy.always_alert {
            let mut states = Vec::with_capacity(raw.len());
            for name in raw {
                match State::parse(&name) {
                    Some(state) => states.push(state),
                    None => warnings.push(format!(
                        "policy.always_alert: unknown state `{name}` — ignored"
                    )),
                }
            }
            c.policy.always_alert = states;
        }
        if let Some(raw) = self.policy.quiet_hours {
            match raw.parse() {
                Ok(q) => c.policy.quiet_hours = Some(q),
                Err(e) => warnings.push(format!("policy.quiet_hours: {e}")),
            }
        }
        if let Some(raw) = self.policy.quiet_hours_action {
            match raw.parse() {
                Ok(a) => c.policy.quiet_hours_action = a,
                Err(e) => warnings.push(format!("policy.quiet_hours_action: {e}")),
            }
        }
        if let Some(v) = self.identity.per_project {
            c.identity.per_project = v;
        }
        if let Some(v) = self.remote.mode {
            c.remote.mode = v;
        }
        if let Some(v) = self.remote.sequences {
            c.remote.sequences = v;
        }
        for (key, raw) in self.sounds {
            match State::parse(&key) {
                Some(state) => {
                    c.sounds.insert(state, expand_home(&raw));
                }
                None => warnings.push(format!(
                    "[sounds] unknown key `{key}` — valid keys are: {}",
                    State::ALL.map(|s| s.as_str()).join(", ")
                )),
            }
        }
        for (key, on) in self.events {
            match State::parse(&key) {
                Some(state) => c.events.set(state, on),
                None => warnings.push(format!(
                    "[events] unknown key `{key}` — valid keys are: {}",
                    State::ALL.map(|s| s.as_str()).join(", ")
                )),
            }
        }

        (c, warnings)
    }
}

/// Keys beckon understands, by table. `[events]` is absent on purpose: its keys
/// are state names, validated against [`State`] during materialization.
pub const KNOWN_KEYS: &[(&str, &[&str])] = &[
    (
        "",
        &[
            "pack", "volume", "enabled", "sounds", "policy", "events", "identity", "remote",
        ],
    ),
    (
        "policy",
        &[
            "min_turn_seconds",
            "rate_limit_ms",
            "always_alert",
            "quiet_hours",
            "quiet_hours_action",
        ],
    ),
    ("identity", &["per_project"]),
    ("remote", &["mode", "sequences"]),
];

/// Dotted paths of keys we do not recognize.
///
/// Typos deserve a warning — silently ignoring `min_turn_second` would leave
/// someone convinced they had configured it — but not the loss of every other
/// setting in the same file.
fn unknown_keys(value: &toml::Value) -> Vec<String> {
    let mut unknown = Vec::new();
    let Some(root) = value.as_table() else {
        return unknown;
    };

    for (table_name, allowed) in KNOWN_KEYS {
        let scope = if table_name.is_empty() {
            Some(root)
        } else {
            root.get(*table_name).and_then(|v| v.as_table())
        };
        let Some(scope) = scope else { continue };

        for key in scope.keys() {
            if !allowed.contains(&key.as_str()) {
                unknown.push(if table_name.is_empty() {
                    key.clone()
                } else {
                    format!("{table_name}.{key}")
                });
            }
        }
    }
    unknown
}

/// Read one TOML layer. A missing file is not a problem; a malformed one warns
/// and contributes nothing.
fn read_layer(path: &Path) -> Layer {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // Absent is normal. Anything else — invalid UTF-8, a symlink loop, a
        // directory — is a file the user believes is being read, so say so
        // rather than behaving as though it were not there.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Layer {
                partial: PartialConfig::default(),
                warnings: Vec::new(),
            }
        }
        Err(e) => {
            return Layer {
                partial: PartialConfig::default(),
                warnings: vec![format!("ignoring {}: {e}", path.display())],
            }
        }
    };

    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return Layer {
                partial: PartialConfig::default(),
                warnings: vec![format!(
                    "ignoring {}: {}",
                    path.display(),
                    compact(&e.to_string())
                )],
            }
        }
    };

    let mut warnings: Vec<String> = unknown_keys(&value)
        .into_iter()
        .map(|key| format!("{}: unknown key `{key}` — ignored", path.display()))
        .collect();

    match value.try_into::<PartialConfig>() {
        Ok(partial) => Layer { partial, warnings },
        Err(e) => {
            // A recognized key with the wrong type. Still costs only this layer.
            warnings.push(format!(
                "ignoring {}: {}",
                path.display(),
                compact(&e.to_string())
            ));
            Layer {
                partial: PartialConfig::default(),
                warnings,
            }
        }
    }
}

/// Flatten a multi-line `toml` diagnostic into one useful line.
///
/// The crate puts the location on the first line and the actual diagnosis
/// ("unknown field `packk`") several lines down, separated by ASCII gutter art.
/// Keeping only the first line would hide the one detail the user needs.
fn compact(s: &str) -> String {
    let useful: Vec<&str> = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        // Drop pure gutter art: `|`, `^^^^`, and the numbered source echo.
        .filter(|l| l.chars().any(|c| c.is_alphabetic()))
        .collect();
    match (useful.first(), useful.last()) {
        (Some(first), Some(last)) if first != last => format!("{first} — {last}"),
        (Some(only), _) => only.to_string(),
        _ => s.trim().to_string(),
    }
}

fn env_layer<F>(get: F) -> Layer
where
    F: Fn(&str) -> Option<String>,
{
    let mut partial = PartialConfig::default();
    let mut warnings = Vec::new();

    let mut parsed = |key: &str, out: &mut dyn FnMut(&str) -> Result<(), String>| {
        if let Some(raw) = get(key) {
            if let Err(e) = out(&raw) {
                warnings.push(format!("{key}: {e}"));
            }
        }
    };

    parsed("BECKON_PACK", &mut |v| {
        partial.pack = Some(v.to_string());
        Ok(())
    });
    parsed("BECKON_VOLUME", &mut |v| {
        partial.volume = Some(
            v.parse()
                .map_err(|_| format!("expected a number, got {v:?}"))?,
        );
        Ok(())
    });
    parsed("BECKON_ENABLED", &mut |v| {
        partial.enabled = Some(parse_bool(v)?);
        Ok(())
    });
    parsed("BECKON_MIN_TURN_SECONDS", &mut |v| {
        partial.policy.min_turn_seconds = Some(
            v.parse()
                .map_err(|_| format!("expected an integer, got {v:?}"))?,
        );
        Ok(())
    });
    parsed("BECKON_RATE_LIMIT_MS", &mut |v| {
        partial.policy.rate_limit_ms = Some(
            v.parse()
                .map_err(|_| format!("expected an integer, got {v:?}"))?,
        );
        Ok(())
    });

    Layer { partial, warnings }
}

/// Expand a leading `~/`. Anything else is taken literally.
fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(dirs) = directories::BaseDirs::new() {
            return dirs.home_dir().join(rest);
        }
    }
    PathBuf::from(raw)
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(format!("expected a boolean, got {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::File::create(path)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
    }

    fn home() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::resolve_with(Some(d.path()));
        (d, p)
    }

    fn hm(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn defaults_match_the_spec() {
        let c = Config::default();
        assert_eq!(c.pack, "aurora");
        assert_eq!(c.volume, 0.6);
        assert!(c.enabled);
        assert_eq!(c.policy.min_turn_seconds, 30);
        assert_eq!(c.policy.rate_limit_ms, 1500);
        assert_eq!(
            c.policy.always_alert,
            vec![
                State::NeedsYou,
                State::RateLimited,
                State::IdleWaiting,
                State::Failed
            ]
        );
        assert!(c.policy.quiet_hours.is_none());
        assert_eq!(c.policy.quiet_hours_action, QuietAction::Silence);
        assert!(c.identity.per_project);
        assert_eq!(c.remote.mode, RemoteMode::Auto);
        assert_eq!(c.remote.sequences, vec![Sequence::Bel, Sequence::Osc9]);
    }

    #[test]
    fn default_enabled_events_match_the_spec() {
        let c = Config::default();
        for s in [
            State::Done,
            State::NeedsYou,
            State::Failed,
            State::RateLimited,
            State::IdleWaiting,
        ] {
            assert!(c.events.enabled(s), "{s} should default on");
        }
        for s in [
            State::SubagentDone,
            State::ToolFailed,
            State::Compacting,
            State::SessionStart,
        ] {
            assert!(!c.events.enabled(s), "{s} should default off");
        }
    }

    #[test]
    fn an_absent_config_yields_defaults_without_warnings() {
        let (_d, p) = home();
        let l = Config::load_verbose(&p, None);
        assert_eq!(l.config, Config::default());
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
    }

    #[test]
    fn project_config_overrides_user_config() {
        let (_d, p) = home();
        let proj = tempfile::tempdir().unwrap();
        write(&p.config_file, "pack = \"cipher\"\nvolume = 0.9\n");
        write(&proj.path().join(".beckon.toml"), "pack = \"unit-7\"\n");

        let c = Config::load(&p, Some(proj.path()));
        assert_eq!(c.pack, "unit-7", "project layer must win");
        assert_eq!(
            c.volume, 0.9,
            "user layer must survive where project is silent"
        );
    }

    #[test]
    fn env_overrides_every_file_layer() {
        let (_d, p) = home();
        write(&p.config_file, "pack = \"cipher\"\nvolume = 0.1\n");
        let env = hm(&[("BECKON_PACK", "aurora"), ("BECKON_VOLUME", "0.8")]);
        let l = Config::load_layers(&p, None, |k| env.get(k).cloned());
        assert_eq!(l.config.pack, "aurora");
        assert_eq!(l.config.volume, 0.8);
    }

    #[test]
    fn partial_config_keeps_defaults_for_unmentioned_keys() {
        let (_d, p) = home();
        write(&p.config_file, "[events]\ntool-failed = true\n");
        let c = Config::load(&p, None);
        assert!(c.events.enabled(State::ToolFailed), "overridden");
        assert!(c.events.enabled(State::Done), "default preserved");
        assert!(!c.events.enabled(State::Compacting), "default preserved");
        assert_eq!(c.pack, "aurora");
        assert_eq!(c.policy.min_turn_seconds, 30);
    }

    #[test]
    fn a_partial_policy_table_keeps_the_other_policy_defaults() {
        let (_d, p) = home();
        write(&p.config_file, "[policy]\nmin_turn_seconds = 5\n");
        let c = Config::load(&p, None);
        assert_eq!(c.policy.min_turn_seconds, 5);
        assert_eq!(c.policy.rate_limit_ms, 1500);
        assert_eq!(c.policy.always_alert.len(), 4);
    }

    #[test]
    fn a_malformed_config_warns_and_falls_back_to_defaults() {
        // Failing open matters: a typo must not silence the tool mysteriously,
        // and must never make the hook exit non-zero.
        let (_d, p) = home();
        write(&p.config_file, "this is not valid toml {{{");
        let l = Config::load_verbose(&p, None);
        assert_eq!(l.config.pack, "aurora");
        assert!(l.config.enabled);
        assert_eq!(l.warnings.len(), 1);
        assert!(l.warnings[0].contains("config.toml"), "{:?}", l.warnings);
    }

    #[test]
    fn an_unknown_key_warns_by_name_rather_than_being_swallowed() {
        let (_d, p) = home();
        write(&p.config_file, "packk = \"cipher\"\n");
        let l = Config::load_verbose(&p, None);
        assert_eq!(l.config.pack, "aurora");
        assert!(
            l.warnings.iter().any(|w| w.contains("packk")),
            "warning should name the typo: {:?}",
            l.warnings
        );
    }

    #[test]
    fn a_misplaced_key_does_not_discard_the_rest_of_the_layer() {
        // This is the failure that matters most: one typo used to throw away
        // the whole file, so `enabled = false` was ignored and beckon spoke
        // when the user had explicitly asked for silence.
        let (_d, p) = home();
        write(
            &p.config_file,
            "enabled = false\nquiet_hours = \"23:00-08:00\"\n",
        );
        let l = Config::load_verbose(&p, None);
        assert!(
            !l.config.enabled,
            "enabled = false must survive a neighbouring typo"
        );
        assert!(
            l.warnings.iter().any(|w| w.contains("quiet_hours")),
            "the misplaced key should still be reported: {:?}",
            l.warnings
        );
    }

    #[test]
    fn unknown_keys_are_reported_with_their_table() {
        let (_d, p) = home();
        write(&p.config_file, "[policy]\nmin_turn_second = 5\n");
        let l = Config::load_verbose(&p, None);
        assert!(
            l.warnings
                .iter()
                .any(|w| w.contains("policy.min_turn_second")),
            "{:?}",
            l.warnings
        );
        assert_eq!(l.config.policy.min_turn_seconds, 30, "default preserved");
    }

    #[test]
    fn a_bad_entry_in_always_alert_does_not_drop_the_good_ones() {
        let (_d, p) = home();
        write(
            &p.config_file,
            "[policy]\nalways_alert = [\"needs-you\", \"needsyou\", \"failed\"]\n",
        );
        let l = Config::load_verbose(&p, None);
        assert_eq!(
            l.config.policy.always_alert,
            vec![State::NeedsYou, State::Failed]
        );
        assert!(
            l.warnings.iter().any(|w| w.contains("needsyou")),
            "{:?}",
            l.warnings
        );
    }

    #[test]
    fn a_known_key_with_the_wrong_type_costs_only_its_layer() {
        let (_d, p) = home();
        write(&p.config_file, "volume = \"loud\"\n");
        let l = Config::load_verbose(&p, None);
        assert_eq!(l.config.volume, 0.6);
        assert!(!l.warnings.is_empty());
    }

    #[test]
    fn an_unknown_event_key_warns_and_is_ignored() {
        let (_d, p) = home();
        write(&p.config_file, "[events]\nneeds-me = true\n");
        let l = Config::load_verbose(&p, None);
        assert!(
            l.warnings.iter().any(|w| w.contains("needs-me")),
            "{:?}",
            l.warnings
        );
        assert_eq!(l.config.events, EventsConfig::defaults());
    }

    #[test]
    fn sound_overrides_are_read_from_the_user_config() {
        let (_d, p) = home();
        write(
            &p.config_file,
            "[sounds]\ndone = \"/home/me/ding.wav\"\nneeds-you = \"/home/me/alert.ogg\"\n",
        );
        let c = Config::load(&p, None);
        assert_eq!(c.sounds[&State::Done], Path::new("/home/me/ding.wav"));
        assert_eq!(c.sounds[&State::NeedsYou], Path::new("/home/me/alert.ogg"));
        assert!(!c.sounds.contains_key(&State::Failed));
    }

    #[test]
    fn a_leading_tilde_is_expanded() {
        let (_d, p) = home();
        write(&p.config_file, "[sounds]\ndone = \"~/sounds/ding.wav\"\n");
        let path = Config::load(&p, None).sounds[&State::Done].clone();
        assert!(path.is_absolute(), "{path:?} was not expanded");
        assert!(path.ends_with("sounds/ding.wav"));
        assert!(!path.to_string_lossy().contains('~'));
    }

    #[test]
    fn a_project_config_may_not_set_sound_files() {
        // The security boundary: a repository you clone must not be able to
        // point your machine at arbitrary files and have them fed to a decoder.
        let (_d, p) = home();
        let proj = tempfile::tempdir().unwrap();
        write(
            &proj.path().join(".beckon.toml"),
            "[sounds]\ndone = \"/etc/shadow\"\n",
        );

        let loaded = Config::load_verbose(&p, Some(proj.path()));
        assert!(
            loaded.config.sounds.is_empty(),
            "project config set a sound file"
        );
        assert!(
            loaded.warnings.iter().any(|w| w.contains("[sounds]")),
            "the refusal must be visible: {:?}",
            loaded.warnings
        );
    }

    #[test]
    fn a_project_config_can_still_change_everything_else() {
        // Only file paths are withheld; the rest of the file still applies.
        let (_d, p) = home();
        let proj = tempfile::tempdir().unwrap();
        write(
            &proj.path().join(".beckon.toml"),
            "enabled = false\npack = \"cipher\"\n[sounds]\ndone = \"/etc/shadow\"\n",
        );
        let c = Config::load(&p, Some(proj.path()));
        assert!(!c.enabled);
        assert_eq!(c.pack, "cipher");
        assert!(c.sounds.is_empty());
    }

    #[test]
    fn a_project_config_cannot_override_a_user_sound() {
        let (_d, p) = home();
        let proj = tempfile::tempdir().unwrap();
        write(&p.config_file, "[sounds]\ndone = \"/home/me/mine.wav\"\n");
        write(
            &proj.path().join(".beckon.toml"),
            "[sounds]\ndone = \"/etc/shadow\"\n",
        );

        let c = Config::load(&p, Some(proj.path()));
        assert_eq!(c.sounds[&State::Done], Path::new("/home/me/mine.wav"));
    }

    #[test]
    fn an_unknown_state_in_sounds_warns_and_is_dropped() {
        let (_d, p) = home();
        write(
            &p.config_file,
            "[sounds]\nneedsyou = \"/x.wav\"\ndone = \"/y.wav\"\n",
        );
        let loaded = Config::load_verbose(&p, None);
        assert!(
            loaded.warnings.iter().any(|w| w.contains("needsyou")),
            "{:?}",
            loaded.warnings
        );
        assert_eq!(loaded.config.sounds.len(), 1, "the good entry must survive");
    }

    #[test]
    fn volume_is_clamped_to_a_sane_range() {
        let (_d, p) = home();
        write(&p.config_file, "volume = 47.0\n");
        assert_eq!(Config::load(&p, None).volume, 1.0);
        write(&p.config_file, "volume = -3.0\n");
        assert_eq!(Config::load(&p, None).volume, 0.0);
    }

    #[test]
    fn quiet_hours_parse_and_wrap_across_midnight() {
        let q: QuietHours = "23:00-08:00".parse().unwrap();
        assert!(q.contains(NaiveTime::from_hms_opt(23, 30, 0).unwrap()));
        assert!(q.contains(NaiveTime::from_hms_opt(2, 0, 0).unwrap()));
        assert!(q.contains(NaiveTime::from_hms_opt(7, 59, 0).unwrap()));
        assert!(!q.contains(NaiveTime::from_hms_opt(12, 0, 0).unwrap()));
        assert!(!q.contains(NaiveTime::from_hms_opt(22, 59, 0).unwrap()));
        assert!(
            !q.contains(NaiveTime::from_hms_opt(8, 0, 0).unwrap()),
            "end is exclusive"
        );
    }

    #[test]
    fn quiet_hours_within_one_day_do_not_wrap() {
        let q: QuietHours = "09:00-17:00".parse().unwrap();
        assert!(q.contains(NaiveTime::from_hms_opt(12, 0, 0).unwrap()));
        assert!(!q.contains(NaiveTime::from_hms_opt(3, 0, 0).unwrap()));
        assert!(
            q.contains(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
            "start is inclusive"
        );
    }

    #[test]
    fn malformed_quiet_hours_are_rejected() {
        for bad in [
            "",
            "23:00",
            "23:00-",
            "-08:00",
            "25:00-08:00",
            "23h-08h",
            "23:00–08:00",
        ] {
            assert!(
                bad.parse::<QuietHours>().is_err(),
                "{bad:?} should not parse"
            );
        }
    }

    #[test]
    fn quiet_hours_action_parses_both_forms() {
        assert_eq!(
            "silence".parse::<QuietAction>().unwrap(),
            QuietAction::Silence
        );
        assert_eq!(
            "volume:0.2".parse::<QuietAction>().unwrap(),
            QuietAction::Volume(0.2)
        );
        assert!("volume:".parse::<QuietAction>().is_err());
        assert!("volume:loud".parse::<QuietAction>().is_err());
        assert!("shout".parse::<QuietAction>().is_err());
    }

    #[test]
    fn a_full_config_file_round_trips_every_field() {
        let (_d, p) = home();
        write(
            &p.config_file,
            r#"
pack = "cipher"
volume = 0.4
enabled = false

[policy]
min_turn_seconds = 12
rate_limit_ms = 400
always_alert = ["needs-you", "failed"]
quiet_hours = "22:30-07:15"
quiet_hours_action = "volume:0.15"

[events]
done = false
compacting = true

[identity]
per_project = false

[remote]
mode = "both"
sequences = ["bel", "osc777"]
"#,
        );
        let l = Config::load_verbose(&p, None);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        let c = l.config;
        assert_eq!(c.pack, "cipher");
        assert_eq!(c.volume, 0.4);
        assert!(!c.enabled);
        assert_eq!(c.policy.min_turn_seconds, 12);
        assert_eq!(c.policy.rate_limit_ms, 400);
        assert_eq!(c.policy.always_alert, vec![State::NeedsYou, State::Failed]);
        assert_eq!(
            c.policy.quiet_hours.unwrap().start,
            NaiveTime::from_hms_opt(22, 30, 0).unwrap()
        );
        assert_eq!(c.policy.quiet_hours_action, QuietAction::Volume(0.15));
        assert!(!c.events.enabled(State::Done));
        assert!(c.events.enabled(State::Compacting));
        assert!(c.events.enabled(State::NeedsYou), "untouched default");
        assert!(!c.identity.per_project);
        assert_eq!(c.remote.mode, RemoteMode::Both);
        assert_eq!(c.remote.sequences, vec![Sequence::Bel, Sequence::Osc777]);
    }

    #[test]
    fn env_booleans_and_numbers_are_parsed_leniently_but_bad_values_warn() {
        let (_d, p) = home();
        let env = hm(&[
            ("BECKON_ENABLED", "false"),
            ("BECKON_MIN_TURN_SECONDS", "7"),
        ]);
        let l = Config::load_layers(&p, None, |k| env.get(k).cloned());
        assert!(!l.config.enabled);
        assert_eq!(l.config.policy.min_turn_seconds, 7);

        let bad = hm(&[("BECKON_VOLUME", "loud")]);
        let l = Config::load_layers(&p, None, |k| bad.get(k).cloned());
        assert_eq!(l.config.volume, 0.6, "falls back to default");
        assert!(
            l.warnings.iter().any(|w| w.contains("BECKON_VOLUME")),
            "{:?}",
            l.warnings
        );
    }
}
