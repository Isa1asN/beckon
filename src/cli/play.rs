//! `beckon __play` — the detached child that actually makes noise.
//!
//! Split from the hook so the agent never waits on audio: the hook decides in
//! milliseconds, spawns this, and exits while the sound is still playing.

use crate::audio::{out, sample, synth, synth::Pcm};
use crate::core::config::Config;
use crate::core::event::State;
use crate::core::paths::{self, Paths};
use crate::pack::{
    manifest::{Pack, SoundDef},
    resolve::{resolve_with_overrides, Source},
    store,
};
use crate::trace::trace;

pub fn run(pack_id: &str, state: State, volume: f32, transpose: f32) {
    let paths = Paths::resolve();
    let cwd = std::env::current_dir().unwrap_or_default();
    let config = Config::load(&paths, Some(&paths::project_root(&cwd)));

    let Some((pack, _)) = store::load(&paths, pack_id) else {
        trace(&format!("sound skipped: no pack `{pack_id}`"));
        return;
    };
    let Some((source_state, source)) = resolve_with_overrides(&pack, &config.sounds, state) else {
        trace(&format!("sound skipped: nothing defined for {state}"));
        return;
    };

    // Claimed before rendering, so a burst that will not be heard does not pay
    // to be generated either. Held until this process exits.
    let Some(_slot) = crate::core::state::claim_player_slot(&paths) else {
        trace(&format!(
            "sound {source_state} skipped: {} already playing",
            crate::core::state::MAX_CONCURRENT_PLAYERS
        ));
        return;
    };
    let Some(pcm) = build(&pack, source, transpose) else {
        return;
    };
    let backend = out::play(&pcm, volume);
    trace(&format!("sound {source_state} via {backend}"));
}

/// Turn a resolved source into playable samples, or trace why not.
///
/// Every failure here is silent-and-explained rather than loud: a broken sample
/// should cost you one missing chime, not an error in your agent's terminal.
pub fn build(pack: &Pack, source: Source<'_>, transpose: f32) -> Option<Pcm> {
    match source {
        Source::Pack(SoundDef::Synth(def)) => Some(synth::render(def, transpose)),

        Source::Pack(SoundDef::Sample(def)) => {
            let Some(root) = pack.root.as_deref() else {
                // A built-in is embedded text and has nowhere to keep a file.
                trace("sound skipped: a built-in pack cannot carry sample files");
                return None;
            };
            let path = match sample::resolve_in_pack(root, &def.file) {
                Ok(path) => path,
                Err(e) => {
                    trace(&format!("sound skipped: {e}"));
                    return None;
                }
            };
            let mut pcm = load_traced(&path)?;
            pcm.samples.iter_mut().for_each(|s| *s *= def.gain);
            Some(shift(pcm, transpose))
        }

        Source::File(path) => Some(shift(load_traced(path)?, transpose)),
    }
}

fn load_traced(path: &std::path::Path) -> Option<Pcm> {
    match sample::load(path) {
        Ok(pcm) => Some(pcm),
        Err(e) => {
            trace(&format!("sound skipped: {e}"));
            None
        }
    }
}

/// Per-project identity for a recorded sound is a playback-rate change, clamped
/// so a sample stays recognisable rather than turning into a chipmunk.
fn shift(mut pcm: Pcm, transpose: f32) -> Pcm {
    pcm.sample_rate = sample::shifted_rate(pcm.sample_rate, transpose);
    pcm
}
