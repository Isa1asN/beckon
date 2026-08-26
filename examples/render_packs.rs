//! Render every built-in pack to WAV so you can hear them.
//!
//!     cargo run --release --example render_packs -- /tmp/beckon-sounds
//!
//! Then listen, e.g. `paplay /tmp/beckon-sounds/aurora/needs-you.wav`.
//!
//! No assertion can tell you whether `needs-you` reads as urgent. This is how
//! you find out.

use beckon_cli::audio::{synth::render, wav};
use beckon_cli::core::event::State;
use beckon_cli::pack::{builtin, manifest::SoundDef, resolve::resolve};

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "./beckon-sounds".to_string());
    let transpose: f32 = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(0.0);

    let mut count = 0usize;
    for pack in builtin::all() {
        for state in State::ALL {
            let Some((source, def)) = resolve(&pack, state) else {
                continue;
            };
            let SoundDef::Synth(synth) = def else {
                continue;
            };
            let pcm = render(synth, transpose);
            let path = std::path::Path::new(&out)
                .join(&pack.meta.id)
                .join(format!("{state}.wav"));
            match wav::write(&path, &pcm) {
                Ok(()) => {
                    let note = if source == state {
                        String::new()
                    } else {
                        format!("  (via {source})")
                    };
                    println!("{:>7.0}ms  {}{note}", pcm.duration_ms(), path.display());
                    count += 1;
                }
                Err(e) => eprintln!("failed to write {}: {e}", path.display()),
            }
        }
    }
    println!("\n{count} files under {out}");
}
