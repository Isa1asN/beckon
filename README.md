# beckon

[![CI](https://github.com/Isa1asN/beckon/actions/workflows/ci.yml/badge.svg)](https://github.com/Isa1asN/beckon/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/beckon-cli.svg)](https://crates.io/crates/beckon-cli)

Distinct sounds for your AI coding agent, so you don't have to watch the
terminal.

beckon binds to your agent's lifecycle hooks and plays a different sound
depending on what it needs:

| Sound | Meaning | What to do |
|---|---|---|
| rising chime | finished its turn | go look |
| insistent sting | blocked on your decision | go unblock it |
| falling tone | it failed | go read the error |
| slow pulse | rate-limited or throttled | wait |

A single undifferentiated ding tells you something happened. It doesn't tell you
whether to get up.

## Install

```bash
cargo install beckon-cli   # the binary is called `beckon`
beckon init                # binds the hooks — shows a diff and asks first
beckon test                # hear the active pack
```

Or from source: `git clone https://github.com/Isa1asN/beckon && cd beckon && cargo install --path .`

`init` prints the exact change it will make to `~/.claude/settings.json`, copies
the file to `settings.json.beckon-backup-<timestamp>` beside itself, and only
then writes. `beckon uninstall` removes beckon's entries and leaves everything
else alone.

It refuses to touch a settings file it cannot round-trip: invalid JSON, invalid
UTF-8, a non-object root, or an unfamiliar `hooks` shape. A file it does accept
is re-serialised, so non-canonical formatting (CRLF, tabs, duplicate keys) is
normalised. Content is preserved; the backup keeps the original bytes.

## Platform support

| | Builds | Test suite | Audio verified |
|---|---|---|---|
| Linux x86-64 | yes | yes | yes |
| macOS | yes | in CI | no |
| Windows | yes | in CI | no |

CI runs the full suite on all three. CI runners have no sound device, so
playback there goes through the null backend — the macOS and Windows audio paths
compile and are exercised structurally, but nobody has heard them. Reports
welcome.

Also built without the `embedded-audio` feature, which is the shape a fully
static musl binary takes: no cpal, no libasound, falling back to a system
player.

Requires Claude Code. Other agents are planned; the adapter seam exists and is
documented in [docs/DESIGN.md](docs/DESIGN.md).

## Commands

```bash
beckon test                     # play every sound in the active pack
beckon packs                    # list packs
beckon use cipher               # switch pack
beckon mute 30m                 # quiet for a while (45s, 2h, …); unmute ends it
beckon doctor                   # why is it quiet? every reason, listed
beckon config set volume 0.4
beckon uninstall
```

## When it stays quiet

By default beckon:

- says nothing when a turn finished in under 30 seconds — you were still watching
- always plays a blocking alert, however fast it arrived, because a permission
  prompt stalls progress
- doesn't repeat the same sound for the same session within 1.5 seconds
- stays quiet for a tool failure, since a failing test is normal work
  (`beckon config set events.tool-failed true` to change that)
- ignores a tool you interrupted yourself
- caps concurrent sounds at 8

If it's quiet and you didn't ask it to be, `beckon doctor` says why.

## Several agents at once

Each project gets a stable transposition from a consonant scale, so
`api-server` and `worktree-auth` sound different with the same pack, and two
sounding together harmonise rather than clash. Nothing to configure; disable
with `beckon config set identity.per_project false`.

Rate limiting is scoped per session and per state. A machine-wide throttle would
let one agent's completion chime swallow another's permission alert.

## Packs

Three ship inside the binary, all original, all CC0:

- **aurora** — calm starship computer. Soft triangle arpeggios, long reverb.
- **cipher** — stealth-game alert. Short, dry, cuts through.
- **unit-7** — deadpan lab robot. Mechanical FM bleeps, no reverb.

A pack is a TOML file, not a folder of audio. Sounds are synth recipes —
oscillators, envelopes, filters — about a kilobyte of text:

```toml
[sounds.done]
type = "synth"
reverb = { room = 0.55, mix = 0.34 }

[[sounds.done.layer]]
wave = "triangle"
notes = ["C5", "E5", "G5"]
step_ms = 92
filter = { kind = "lowpass", cutoff_hz = 3200 }
```

So a pack is provably original, weighs nothing, and can be reviewed as a diff.
Auditing a folder of binary blobs for licence provenance is what makes shared
sound libraries impractical.

To write one, drop a `pack.toml` in `~/.local/share/beckon/packs/<id>/`. It
shadows a built-in of the same name, so you can fork `aurora` and keep the name.
`beckon test <id>` to hear it.

## Your own sounds

You don't need to author a pack to use your own audio:

```bash
beckon config set sounds.needs-you ~/sounds/alert.wav
```

Anything you don't override falls through to the active pack, at every step of
the fallback chain — replace `failed` and `rate-limited` follows it. wav, ogg,
flac and mp3 are supported. The path is checked when you set it, so a typo fails
immediately rather than becoming silence you notice days later.

Or write the table directly:

```toml
# ~/.config/beckon/config.toml
[sounds]
needs-you = "~/sounds/alert.wav"
done      = "~/sounds/ding.wav"
```

`[sounds]` is honoured only in your own config, never in a project's
`.beckon.toml`. A repository you clone can change *when* beckon makes a noise;
it cannot name files on your machine and have them opened by a media decoder.

Sample files are bounded: regular files only, 10 MiB and 30 seconds maximum, and
a pack's samples must resolve inside the pack after symlinks are followed.

## Footprint

- No daemon. Nothing runs between hooks. Your agent invokes beckon, it decides
  in ~5ms, hands playback to a detached child, and exits.
- No network at hook time.
- No telemetry.
- Packs are data, never executed.
- Exits 0 unconditionally — bad config, no audio device, corrupt input, panic.
  beckon binds hooks that block the agent on a non-zero exit, so this is
  verified against the release binary, where `panic = "abort"` puts it beyond
  the reach of `cargo test`.

## Roadmap

- [x] Publish to crates.io
- [ ] Hear it on macOS and Windows (CI builds and tests there already)
- [ ] `beckon install github:user/repo` — packs from git
- [ ] A browsable community pack index
- [ ] SSH: escape sequences so a remote agent alerts your local terminal
- [ ] Adapters for Codex, Cursor, Gemini
- [ ] npm and Homebrew distribution

## Contributing

```bash
git clone https://github.com/Isa1asN/beckon && cd beckon
./scripts/install-hooks.sh   # pre-commit: fmt + clippy
./check.sh --release         # everything CI enforces
```

`main` requires a pull request and **signed commits**. If you don't already
sign, GitHub will reject the push without much explanation:

```bash
git config --global gpg.format ssh
git config --global user.signingkey ~/.ssh/id_ed25519.pub
git config --global commit.gpgsign true
```

Then add the same key to GitHub under Settings → SSH and GPG keys with key type
**Signing Key** — an authentication key does not count, and that catches most
people out.

CI runs fmt, clippy with `-D warnings`, the suite on Linux/macOS/Windows, a
build with no audio backend, an MSRV check, the release-binary safety script,
and crates.io packaging. `./check.sh --release` covers everything except the
other two platforms.

New sound packs are welcome — a pack is a TOML file, so a pull request adding
one is reviewable as a diff. Design notes: [docs/DESIGN.md](docs/DESIGN.md).

## Licence

Code is MIT OR Apache-2.0. The built-in packs are CC0-1.0.
