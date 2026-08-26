//! Decision tracing.
//!
//! When `BECKON_TRACE` names a file, every decision is appended to it. This is
//! the seam the integration suite observes instead of listening for audio, and
//! it is the first thing `beckon doctor` suggests when someone asks "why didn't
//! it make a sound?".
//!
//! Deliberately not stderr: during a hook, stderr is the agent's terminal.

use std::io::Write;

pub fn trace(message: &str) {
    let Some(path) = std::env::var_os("BECKON_TRACE") else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };

    // One `write` call, not two. `writeln!` emits the message and its newline
    // separately, and with several agents running — the workload beckon exists
    // for — those interleave: measured 13 of 30 lines mangled. The log `doctor`
    // recommends has to be readable precisely when things are busiest.
    let mut line = String::with_capacity(message.len() + 1);
    line.push_str(message);
    line.push('\n');
    let _ = file.write_all(line.as_bytes());
}
