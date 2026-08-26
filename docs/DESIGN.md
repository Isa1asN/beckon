# beckon — design

How beckon is put together and why. Companion to the README, which covers
using it; this covers the decisions behind it.

## 1. What it is

A small CLI that gives AI coding agents a voice. It wires itself into an agent's
lifecycle hooks and plays a *semantically distinct* sound depending on what the
agent needs: finished, blocked on you, failed, throttled. Sound packs supply the
personality; the tool supplies the meaning.

The value is not "play a sound when done" — that is a ten-line gist. The value is
a normalized event vocabulary that means the same thing across agents whose hook
surfaces are wildly uneven, plus a pack format that makes a community sound
library reviewable and legally clean.

**Name:** product and binary are `beckon`. Published as `beckon-cli` on crates.io
and npm (`beckon` is taken on crates.io by an unrelated HTTP client, and squatted
on npm by an abandoned 2022 stub).

## 2. Non-goals

- Not a general notification framework. Sound and terminal escape sequences only.
- Not a TTS engine. Packs are synth recipes or audio files; speech is out of v0.1.
- Not a daemon. No background process, no socket, no lifecycle to manage.
- No telemetry. No network access at hook time, ever.
- No GUI, no tray icon, no web dashboard in v0.1.

## 3. Design principles

These are load-bearing. Violating any of them is a bug, not a trade-off.

1. **Never break the agent.** beckon exits 0 unconditionally — on bad config,
   missing pack, no audio device, panic, or corrupt stdin. Several hooks it binds
   (`Stop`, `UserPromptSubmit`) can *block the agent* on a non-zero exit. A sound
   tool must never be able to wedge someone's session.
2. **Never speak on stdout.** For `UserPromptSubmit` and `SessionStart`, plain
   stdout is injected into the model's context. beckon writes nothing to stdout
   except a single well-formed JSON object, and only on events where that is
   safe.
3. **Off the hot path.** beckon binds turn-boundary and failure events only —
   never `PreToolUse`/`PostToolUse`. A typical session fires 5–20 hooks total,
   not thousands.
4. **Don't make the agent wait.** The hook process decides and spawns a detached
   child to render and play, then exits in single-digit milliseconds.
5. **Packs are data.** Never executed, never trusted with paths, never fetched at
   hook time.
6. **Reversible install.** Every config edit is previewable, backed up, and
   removable by `beckon uninstall` with byte-level precision about what it owns.

## 4. Architecture

```
beckon (single static Rust binary, no runtime deps)
├── cli/          init · uninstall · doctor · test · packs · install · use
│                 mute · config · hook (internal) · __play (internal)
├── adapter/      agent hook payload  →  normalized Event
│                 claude_code.rs   (v0.1)
│                 (codex · cursor · gemini · opencode — later)
├── core/
│   ├── event.rs    the normalized vocabulary
│   ├── policy.rs   should this make a sound? (gates, limits, mute, quiet hours)
│   ├── state.rs    per-session turn timestamps; mute state
│   └── config.rs   layered config resolution
├── pack/
│   ├── manifest.rs pack.toml parse + validate
│   ├── resolve.rs  event → sound source, with fallback chain
│   └── install.rs  builtin · local dir · git · index
├── audio/
│   ├── synth.rs    recipe → PCM
│   ├── mix.rs      layering, normalization, reverb
│   └── out.rs      rodio/cpal → system player → terminal bell
└── remote/osc.rs   escape sequences for the *local* terminal over SSH
```

### Data flow

```
agent fires hook
  → beckon hook claude-code   (stdin: JSON payload)
      → adapter.parse()       → Event { state, session, project, meta }
      → policy.decide()       → Play(spec) | Suppress(reason)
      → spawn detached: beckon __play <spec>
      → [if remote] print {"terminalSequence": "..."} to stdout
      → exit 0                                            ~5ms
                                     detached child:
                                       pack.resolve(state) → SoundSource
                                       synth render | decode sample
                                       normalize → gain → identity transpose
                                       audio.out.play()                ~400ms
```

## 5. The event vocabulary

The core three are **required** in every pack. Extended states are optional; a
pack that omits one falls back down the chain, and silence is a valid terminus.
This keeps the authoring floor at three sounds while allowing depth.

| State | Meaning | Fallback |
|---|---|---|
| `done` | Turn finished; ball is in your court | — (required) |
| `needs-you` | Blocked awaiting a human decision | — (required) |
| `failed` | Turn ended badly | — (required) |
| `rate-limited` | API throttle, overload, billing, auth failure | `failed` |
| `idle-waiting` | Agent has been waiting on you a while | `needs-you` |
| `subagent-done` | A subagent finished | silence |
| `compacting` | Context compaction starting | silence |
| `session-start` | Session opened | silence |
| `tool-failed` | An individual tool call failed | silence |

`subagent-done` deliberately does *not* fall back to `done` — hearing the "come
look" chime for something that isn't finished trains people to ignore it.

### Claude Code mapping

| Hook event | Discriminator | → state | Default |
|---|---|---|---|
| `UserPromptSubmit` | — | *(no sound; records turn start)* | on |
| `Stop` | — | `done` | on, duration-gated |
| `Notification` | `permission_prompt` | `needs-you` | on, ungated |
| `Notification` | `agent_needs_input`, `elicitation_dialog` | `needs-you` | on, ungated |
| `Notification` | `idle_prompt` | `idle-waiting` | on, ungated |
| `Notification` | `agent_completed` | `done` | on |
| `StopFailure` | `rate_limit`, `overloaded`, `billing_error`, `authentication_failed` | `rate-limited` | on, ungated |
| `StopFailure` | any other | `failed` | on, ungated |
| `PostToolUseFailure` | — | `tool-failed` | **off** |
| `SubagentStop` | — | `subagent-done` | **off** |
| `PreCompact` | — | `compacting` | **off** |
| `SessionStart` | — | `session-start` | **off** |
| `SessionEnd` | — | *(no sound; prunes session state)* | on |

`tool-failed` defaults off on purpose: a failing test is normal work, and chiming
on every non-zero exit is the fastest route to being uninstalled.

Routing happens *inside* beckon by reading `hook_event_name`, `notification_type`
and `stop_reason` from stdin — not via matchers in settings.json. That keeps the
config diff to nine readable entries (§10) and lets routing change without rewriting
the user's settings file.

### Adding an agent

An adapter implements:

```rust
trait Adapter {
    fn id(&self) -> &'static str;
    fn parse(&self, stdin: &[u8]) -> Option<Event>;
    fn install_plan(&self, scope: Scope) -> Result<InstallPlan>;
    fn uninstall_plan(&self, scope: Scope) -> Result<InstallPlan>;
}
```

Known shapes for later adapters: Codex exposes a single `notify` program in
`~/.codex/config.toml` (turn-complete only — maps to `done`, no `needs-you`);
Cursor has a `hooks.json`; Aider has `--notifications-command`. Agents that only
signal completion get `done` and nothing else, and `beckon doctor` states plainly
which states a given agent can and cannot produce. Honest degradation beats
faking distinctions the agent never gave us.

## 6. Policy — when beckon stays silent

Ordered checks; first match wins.

1. `enabled = false`, or muted (`beckon mute 30m`) → silent.
2. Quiet hours window → silent, or reduced volume if configured.
3. Event disabled in `[events]` → silent.
4. **Rate limit:** less than `rate_limit_ms` (default 1500) since **this
   session** last played **this same state** → silent.

   The key is `(session, state)`. A machine-wide throttle looks reasonable
   until agents run in parallel: their turn boundaries are correlated, so four
   agents hitting permission prompts together collapse to a single chime, and
   one agent's completion silences another's alert. A *different* state always
   carries new information, and so does the same state from a *different*
   agent. The only burst worth collapsing is the same sound from the same
   session — `Stop` and `Notification/agent_completed` both mapping to `done`,
   or a parallel tool batch producing several `tool-failed`.

   Because a burst arrives concurrently, the check and its update are taken
   under an advisory lock; otherwise every process reads the same empty history
   before any of them writes, and nothing is collapsed at all.


5. **Duration gate:** for any state *not* listed in `always_alert`, if the turn
   ran shorter than `min_turn_seconds` (default 30), stay silent — a short turn
   means you were watching. In practice this gates `done` and `subagent-done`;
   everything blocking or broken is in `always_alert` by default.

   Note that `subagent-done` is gated against the *parent* turn, because a
   subagent never emits `UserPromptSubmit` and so has no start of its own. That
   is the intended reading rather than a limitation: whether you have tabbed
   away is governed by how long the whole turn has been running, not by how long
   one subagent took. The rate limit
   in step 4 still applies to every state, alerts included — `always_alert`
   bypasses the duration gate only.
6. Otherwise → play.

The gate needs turn duration, which no hook provides. `UserPromptSubmit` writes a
timestamp to `state/sessions/<session_id>.json`; `Stop` reads it back. **If the
state file is missing, beckon plays.** Failing open matters: beckon installed
mid-session, or a resumed session, must not be mysteriously silent.

State is one small file per session — no shared-file contention when four agents
run in parallel worktrees, and no machine-wide file that would couple them. Each
session's file holds its turn start and the last time it played each state, so
the rate limit in §6 is scoped correctly by construction. Writes are atomic
(temp file plus rename).

Sessions untouched for 7 days are pruned opportunistically, by `stat` rather than
by reading each file: pruning runs on `UserPromptSubmit`, which is one of the
hooks that can block the agent, and a full-read scan cost 28ms with a thousand
session files against 9ms for a stat scan. Temp files orphaned by a crash between
write and rename are collected by the same pass.

## 7. Pack format

`pack.toml`, a directory, and nothing else. No scripts, no install steps.

```toml
[pack]
id          = "aurora"
name        = "Aurora"
version     = "1.0.0"
author      = "esayas-beshah"
license     = "CC0-1.0"      # SPDX identifier, required
description = "A calm starship computer."
beckon      = "^0"           # format compatibility

[sounds.done]
type = "synth"
gain = 0.9

  [[sounds.done.layer]]
  wave       = "triangle"
  notes      = ["C5", "E5", "G5"]   # a sequence is an arpeggio
  step_ms    = 90
  dur_ms     = 260
  attack_ms  = 5
  decay_ms   = 40
  sustain    = 0.7
  release_ms = 180
    [sounds.done.layer.filter]
    kind     = "lowpass"
    cutoff_hz = 3200
    q         = 0.7

  [sounds.done.reverb]
  room = 0.35
  mix  = 0.25

[sounds.needs-you]
type    = "sample"           # the other source type
file    = "needs-you.ogg"
license = "CC0-1.0"          # per-file provenance for samples
```

### Synth model

A sound is a gain plus an ordered list of layers, mixed and post-processed.
Deliberately small — enough primitives to compose a starship, a stealth sting and
a lab robot, few enough to hand-write and review in a pull request.

**Layer:** `wave` (`sine`|`triangle`|`square`|`saw`|`noise`|`fm`), `notes`
(a TOML string is a scientific pitch name — `"C5"`, `"A#3"`, `"Eb5"`, A4 = 440 Hz
— and a number is raw Hz; the two may be mixed in one sequence), `step_ms`, `dur_ms`, `delay_ms`, `gain`,
`pan`, ADSR (`attack_ms`/`decay_ms`/`sustain`/`release_ms`), `glide_ms`,
`detune_cents`, optional `fm { ratio, index }`, optional
`filter { kind, cutoff_hz, q }`, optional `vibrato { rate_hz, depth_cents }`.

**Sound-level:** `gain`, optional `reverb { room, mix }`, `normalize` (default
true).

**Loudness:** every sound is peak-normalized to −3 dBFS before pack gain and user
volume are applied. Without this, one loud community pack ruins the experience
and users blame beckon.

### Per-project identity

With `identity.per_project = true` (default), a stable hash of the project root
selects a transposition from a pleasant scale (`[0, +2, +4, +5, +7, +9]`
semitones). The same pack sounds consistently different in `api-server` than in
`worktree-auth`, so parallel agents are distinguishable without separate packs.
For synth sounds this is exact transposition; for samples it is a playback-rate
shift clamped to ±3 semitones to avoid chipmunking. `beckon test --here` plays
the current project's variant.

### Validation

Enforced on install and in CI: manifest schema; SPDX `license` present; all three
core sounds defined; sample paths resolve *inside* the pack directory (no
traversal, no symlinks); audio files are wav/ogg/flac/mp3; pack total ≤ 20 MB; no
executable bits. A pack that fails validation is rejected with the specific
reason, never partially installed.

## 8. Audio output

Chain, in order, first success wins:

1. **Embedded** — `rodio`/`cpal`. Works with no system player installed, which is
   the single most common Linux support burden for tools like this.
2. **System player** — render to a temp WAV and shell out
   (`paplay`/`pw-play`/`aplay`/`afplay`/PowerShell `SoundPlayer`).
3. **Terminal bell** — `\x07`. Something is better than nothing.

`beckon doctor` reports which path is live and why the others were skipped.

Sounds render on the fly; no cache. A few hundred milliseconds of layered
oscillators costs microseconds to synthesize, and a cache would be state to
invalidate for no gain.

## 9. Remote / SSH

> **Designed, not built.** No part of this ships today; a remote agent plays
> audio on the remote machine. Recorded here because the pack format and config
> schema already carry the seams for it (`[remote]`, §14).

When the agent runs on a remote box, playing audio *there* is useless. beckon
detects `SSH_CONNECTION`/`SSH_TTY` and instead emits escape sequences via the
hook's `terminalSequence` output, which travel back to the terminal on your desk.

Default set is conservative — `bel` (`\x07`, universal) and `osc9`
(`\x1b]9;<msg>\x07`, desktop notification in iTerm2, Windows Terminal and
others). `osc777` is opt-in because support is patchier. `remote.mode` accepts `auto`
(sequences only when SSH is detected — the default), `off` (never), `always`
(sequences regardless of SSH, and no local audio) and `both` (local audio *and*
sequences, for a local terminal you want notified too). Per the hook docs,
`terminalSequence` is unsupported on `StopFailure`, so that event is bell-only
when remote.

Shipped as best-effort and documented as such: verified on Ghostty, WezTerm,
Kitty and iTerm2 before v0.1 claims support for any of them.

## 10. Install and trust

`beckon init` binds nine hooks in `~/.claude/settings.json` (or the project
file with `--scope project`):

```
UserPromptSubmit · Stop · Notification · StopFailure · PostToolUseFailure
SubagentStop · PreCompact · SessionStart · SessionEnd
```

Every event that maps to a state in §5 is bound, including those whose state is
disabled by default — so enabling `tool-failed` or `compacting` is a config edit
that takes effect immediately, with no re-install and no footgun. Two of the nine
never make a sound: `UserPromptSubmit` records the turn start for the duration
gate, and `SessionEnd` deletes that session's state file.

Each entry is `beckon hook claude-code` with `timeout: 5`. Before writing, init:

- prints the exact JSON diff and requires confirmation (`--yes` to skip,
  `--dry-run` to print and exit);
- writes `settings.json.beckon-backup-<timestamp>`;
- preserves key order and untouched content (serde_json with `preserve_order`);
- tags its entries so `uninstall` removes exactly its own and nothing else.

`beckon uninstall` restores the file to a state without beckon entries, leaving
every other hook intact, and reports what it removed.

## 11. Built-in packs

Three, all synth, all CC0-1.0, embedded in the binary as TOML — zero download,
zero licensing surface, reviewable as a text diff.

- **aurora** — calm starship computer. Soft triangle/sine arpeggios, low-passed,
  long reverb tail. `done` rises through a major triad; `needs-you` is a gentle
  repeated interrogative; `failed` descends with slight detune.
- **cipher** — stealth-game alert. Short, dry, punchy square/saw with filtered
  noise transients. `needs-you` is the classic two-blip "!" sting.
- **unit-7** — deadpan lab robot. Mechanical FM bleeps, no reverb, clipped
  envelopes. `failed` is a downward glissando.

## 12. Distribution

Only the first of these exists today; the rest are the intended shape.

- `cargo install beckon-cli` — works now
- `npm i -g beckon-cli` — thin wrapper with per-platform optional dependencies
  (the pattern esbuild and Biome use), so the one-command install devs expect
  works without a Rust toolchain. The package installs a binary named `beckon`;
  only the registry name carries the `-cli` suffix.
- `curl … | sh` installer and a Homebrew formula
- GitHub Actions release matrix: linux x64/arm64 (gnu + musl), macOS x64/arm64,
  windows x64

Code is dual MIT / Apache-2.0. Built-in packs are CC0-1.0.

## 13. The registry, without a registry

The intended community layer is a directory in the beckon repo plus a generated
`index.json` — contribution by pull request, validation in CI, no server and no
hosting bill.

Built today:

- `beckon packs`, `beckon use <id>` — list and switch
- a hand-written `pack.toml` under `~/.local/share/beckon/packs/<id>/` is
  discovered automatically and shadows a built-in of the same id

Designed, not built:

- `beckon install ./my-pack` — local directory
- `beckon install github:user/repo[/subdir][@ref]` — git or codeload tarball
- `beckon install aurora-noir` — bare name, resolved through the cached index

Contribution is a pull request adding `packs/<id>/pack.toml`; CI runs the same
validator as `install`. No server, no hosting cost, no moderation backlog — and
because synth packs are text, a reviewer can actually read what they are merging.
A hosted browser is a later sub-project, and only if the library earns one.

## 14. Configuration

Layered, later wins: built-in defaults → user config → `$PROJECT/.beckon.toml` →
`BECKON_*` environment variables.

`$PROJECT` is found by walking up from the agent's working directory to the
nearest directory containing `.beckon.toml` or a VCS marker (`.git`, `.jj`,
`.hg`, `.svn`), falling back to the working directory. Agents are routinely
launched from a subdirectory, and a config at the repository root that silently
does nothing is the worst kind of failure — the user believes they configured
something.

A layer that fails to parse is skipped with a warning, but an *unrecognized key*
never costs the rest of its file. Rejecting the whole document on one typo meant
a misplaced key could discard `enabled = false` and make beckon speak when the
user had asked for silence. Unknown keys are reported by name; unknown entries in
`always_alert` and `[events]` are dropped individually.

```toml
pack    = "aurora"
volume  = 0.6
enabled = true

[policy]
min_turn_seconds  = 30
rate_limit_ms     = 1500
always_alert      = ["needs-you", "rate-limited", "idle-waiting", "failed"]
# quiet_hours     = "23:00-08:00"
# quiet_hours_action = "silence"       # or "volume:0.2"

[events]
done          = true
needs-you     = true
failed        = true
rate-limited  = true
idle-waiting  = true
subagent-done = false
tool-failed   = false
compacting    = false
session-start = false

[identity]
per_project = true

[remote]
mode = "auto"                # auto | off | always | both
sequences = ["bel", "osc9"]  # osc777 opt-in
```

## 15. Testing

- **Adapter:** golden fixtures of real Claude Code hook payloads → expected
  `Event`. Every row of the mapping table in §5 gets a fixture.
- **Policy:** injected clock; each ordered check in §6 gets a test, including the
  fail-open path when session state is missing.
- **Synth:** deterministic render, snapshot-tested by hash. Same recipe must
  produce identical PCM across runs and platforms.
- **Manifest:** valid/invalid corpus, including traversal, symlink, oversize and
  missing-license cases.
- **Install:** golden `settings.json` before/after, plus a round-trip test
  asserting `init` → `uninstall` returns the file to identical content when it
  contained pre-existing unrelated hooks, including hook group shapes beckon
  does not itself write. Byte-for-byte identity holds only for files already in
  canonical JSON; anything else is re-serialised, and the backup is what
  preserves the original bytes.
- **Safety:** garbage, truncated, and empty stdin fed to every hook entry point,
  asserting exit code 0 and empty stdout. Repeated against the *release* binary
  by `scripts/verify-release-safety.sh`, because the release profile sets
  `panic = "abort"` while cargo forces unwinding for test targets — so the
  profile users actually run is one the Rust suite cannot reach.
- **Trust boundaries:** the §16 rules have adversarial tests — traversal,
  symlink escape via a file and via a parent directory, character devices,
  oversized and long-decoding files, and a project config attempting `[sounds]`.
- **Concurrency:** bursts of simultaneous hooks against one session, asserting
  the rate limit collapses them; playback slots capped and released; pruning
  refusing to collect a session under lock.
- **Human:** `beckon doctor` and `beckon test <pack>`. No assertion can tell you
  whether `needs-you` reads as urgent.

## 16. Trust boundaries

Three sources of input are trusted differently. The distinctions are enforced,
not advisory.

**The user's own config** may name any file. It is their machine; refusing
absolute paths would defeat the point of `[sounds]`.

**A project's `.beckon.toml`** arrives with a repository and may change *when*
beckon makes a noise — `enabled`, `pack`, `[events]`, policy — but may **not**
set `[sounds]`. Letting it name files would hand any cloned repository the
ability to have arbitrary paths opened and fed to a media decoder. The refusal
is reported rather than silent.

**A pack manifest** may reference sample files only *inside its own directory*,
verified after canonicalization so a symlink cannot leave. Extensions are
allowlisted, and packs are never executed.

Independently of provenance, every decode is bounded: regular files only (so a
character device or FIFO cannot stall playback), 10 MiB before reading, 30
seconds and an absolute sample ceiling while decoding. Synth rendering is bounded
the same way, on a budget of samples written — capping duration and note count
separately is not enough, because the cost is their product.

Untrusted text is escaped before it reaches a terminal. Pack metadata and config
keys are read from files beckon did not write, and echoing them raw would let
them clear the screen or retitle the window.

## 17. Known limitations

- **Settings files are re-serialised.** beckon refuses shapes it cannot handle —
  invalid JSON or UTF-8, a non-object root, an unfamiliar `hooks` shape — but a
  file it does accept is rewritten in canonical form: CRLF becomes LF,
  indentation normalises, duplicate keys collapse. Content is preserved;
  formatting is not. The backup holds the original bytes.
- **Sound is local.** When the agent runs on a remote machine, audio plays
  there. §9 describes the escape-sequence path; it is best-effort and terminal
  support varies.
- **Only Claude Code ships.** The adapter seam exists and is documented (§5), but
  agents that expose a single "turn complete" callback can never produce the
  distinctions the vocabulary allows. `beckon doctor` states which states a given
  agent can actually produce rather than pretending.
- **A pack cannot be verified, only validated.** Manifest validation checks
  structure, licence presence and path containment. It cannot tell you whether an
  author had the right to the audio they shipped.
