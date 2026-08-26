//! beckon CLI entry point.

#![forbid(unsafe_code)]

use beckon_cli::adapter::Scope;
use beckon_cli::cli;
use beckon_cli::core::event::State;
use beckon_cli::guard::{exit_with, install_panic_guard};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ScopeArg {
    /// Every project, via the agent's user settings.
    User,
    /// This repository only. Can be committed.
    Project,
}

impl From<ScopeArg> for Scope {
    fn from(arg: ScopeArg) -> Scope {
        match arg {
            ScopeArg::User => Scope::User,
            ScopeArg::Project => Scope::Project,
        }
    }
}

#[derive(Parser)]
#[command(name = "beckon", version, about = "Give your AI coding agent a voice.")]
#[command(
    after_help = "Start with `beckon init` to bind the hooks and `beckon test` to hear the \
                  active pack. `beckon mute 30m` when you need quiet, and `beckon doctor` \
                  when something is quiet and you did not ask for it."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print one setting's effective value.
    Get {
        /// Dotted key, e.g. `volume` or `policy.min_turn_seconds`.
        key: String,
    },
    /// Change one setting in your user config.
    Set { key: String, value: String },
    /// Print the path to the user config file.
    Path,
}

#[derive(Subcommand)]
enum Cmd {
    /// Bind beckon to an agent's lifecycle hooks.
    Init {
        /// Which agent to install for.
        #[arg(long, default_value = "claude-code")]
        agent: String,
        /// Write to the agent's user settings, or to this repository's.
        #[arg(long, value_enum, default_value_t = ScopeArg::User)]
        scope: ScopeArg,
        /// Show the diff and exit without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,
    },

    /// Remove beckon's hooks, leaving every other setting intact.
    Uninstall {
        #[arg(long, default_value = "claude-code")]
        agent: String,
        #[arg(long, value_enum, default_value_t = ScopeArg::User)]
        scope: ScopeArg,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, short)]
        yes: bool,
    },

    /// List the available sound packs.
    Packs,

    /// Switch to a different sound pack.
    Use {
        /// Pack id, as shown by `beckon packs`.
        pack: String,
    },

    /// Stay quiet for a while. Defaults to 30m; try 45s, 15m, 2h.
    Mute {
        /// How long, e.g. `45s`, `15m`, `2h`. Bare numbers are minutes.
        duration: Option<String>,
    },

    /// End a mute early.
    Unmute,

    /// Show or change settings.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },

    /// Play a pack's sounds so you can hear them.
    Test {
        /// Pack id. Defaults to the active pack.
        pack: Option<String>,
        /// Play only this state, e.g. `needs-you`.
        #[arg(long, value_name = "STATE")]
        state: Option<String>,
        /// Apply this project's transposition, as you would actually hear it.
        #[arg(long)]
        here: bool,
    },

    /// Explain what beckon will do, and why it might be silent.
    Doctor,

    /// Internal: consume an agent lifecycle hook payload on stdin.
    #[command(hide = true)]
    Hook {
        /// Adapter id, e.g. `claude-code`.
        agent: String,
    },

    /// Internal: detached playback, spawned by `hook`.
    #[command(name = "__play", hide = true)]
    Play {
        #[arg(long)]
        pack: String,
        #[arg(long)]
        state: String,
        #[arg(long, default_value_t = 1.0)]
        volume: f32,
        #[arg(long, default_value_t = 0.0)]
        transpose: f32,
    },
}

fn main() {
    // First statement, before any work can panic.
    install_panic_guard();

    let code = match Cli::try_parse() {
        Ok(cli) => dispatch(cli.command),
        Err(e) => {
            let _ = e.print();
            // The exit-0 guarantee exists because a hook exiting non-zero can
            // block the agent — so it applies to the invocations an agent
            // actually makes, and only those. A person mistyping a subcommand
            // deserves a real exit code they can branch on.
            match e.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => 0,
                _ if invoked_as_hook() => 0,
                _ => 2,
            }
        }
    };

    exit_with(code);
}

/// Is this the agent calling us, rather than a person?
fn invoked_as_hook() -> bool {
    matches!(
        std::env::args().nth(1).as_deref(),
        Some("hook") | Some("__play")
    )
}

fn dispatch(command: Cmd) -> i32 {
    match command {
        Cmd::Init {
            agent,
            scope,
            dry_run,
            yes,
        } => cli::install::init(cli::install::Options {
            agent,
            scope: scope.into(),
            dry_run,
            assume_yes: yes,
        }),
        Cmd::Uninstall {
            agent,
            scope,
            dry_run,
            yes,
        } => cli::install::uninstall(cli::install::Options {
            agent,
            scope: scope.into(),
            dry_run,
            assume_yes: yes,
        }),
        Cmd::Test { pack, state, here } => match parse_state(state) {
            Ok(state) => cli::test_cmd::run(pack, state, here),
            Err(code) => code,
        },
        Cmd::Packs => cli::packs::list(),
        Cmd::Use { pack } => cli::packs::use_pack(&pack),
        Cmd::Mute { duration } => cli::mute::mute(duration),
        Cmd::Unmute => cli::mute::unmute(),
        Cmd::Config { action } => match action {
            None => cli::config_cmd::show(),
            Some(ConfigAction::Get { key }) => cli::config_cmd::get(&key),
            Some(ConfigAction::Set { key, value }) => cli::config_cmd::set(&key, &value),
            Some(ConfigAction::Path) => cli::config_cmd::path(),
        },
        Cmd::Doctor => cli::doctor::run(),
        Cmd::Hook { agent } => {
            run_hook(&agent);
            0
        }
        Cmd::Play {
            pack,
            state,
            volume,
            transpose,
        } => {
            // Spawned by us, so an unparseable state is a bug, not user input.
            if let Some(state) = State::parse(&state) {
                cli::play::run(&pack, state, volume, transpose);
            }
            0
        }
    }
}

fn parse_state(raw: Option<String>) -> Result<Option<State>, i32> {
    match raw {
        None => Ok(None),
        Some(name) => match State::parse(&name) {
            Some(state) => Ok(Some(state)),
            None => {
                eprintln!(
                    "unknown state `{name}`. Valid states: {}",
                    State::ALL.map(|s| s.as_str()).join(", ")
                );
                Err(2)
            }
        },
    }
}

fn run_hook(agent: &str) {
    if std::env::var_os("BECKON_PANIC_TEST").is_some() {
        panic!("induced panic for testing the exit-0 guarantee");
    }
    cli::hook::run(agent);
}
