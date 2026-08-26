#!/usr/bin/env bash
# The exit-0 guarantee, verified against the RELEASE binary.
#
# Why this exists as a shell script rather than a #[test]: the release profile
# sets `panic = "abort"`, but cargo forces `panic = "unwind"` for test targets
# even under `cargo test --release`. So the profile users actually run is the
# one the Rust test suite can never exercise. This closes that hole.
set -uo pipefail

BIN=${1:-./target/release/beckon}
fails=0

check() {
    local label="$1" expected_exit="$2" expected_stdout="$3"
    shift 3
    local stdout status
    stdout=$("$@" 2>/dev/null </dev/null || true)
    status=$?
    # Re-run capturing status properly (command substitution masks it above).
    "$@" >/dev/null 2>&1 </dev/null
    status=$?

    if [[ "$status" != "$expected_exit" ]]; then
        echo "  ✗ $label: exit $status, expected $expected_exit"
        fails=$((fails + 1))
    elif [[ "$expected_stdout" == "empty" && -n "$stdout" ]]; then
        echo "  ✗ $label: wrote to stdout: $stdout"
        fails=$((fails + 1))
    else
        echo "  ✓ $label"
    fi
}

pipe_check() {
    local label="$1" payload="$2"
    local stdout status
    stdout=$(printf '%s' "$payload" | "$BIN" hook claude-code 2>/dev/null)
    status=$?
    if [[ "$status" != 0 ]]; then
        echo "  ✗ $label: exit $status, expected 0"
        fails=$((fails + 1))
    elif [[ -n "$stdout" ]]; then
        echo "  ✗ $label: wrote to stdout: $stdout"
        fails=$((fails + 1))
    else
        echo "  ✓ $label"
    fi
}

# Like pipe_check, but the payload is a printf format string, so control
# characters and NUL survive.
raw_check() {
    local label="$1" fmt="$2"
    shift 2
    local stdout
    stdout=$(printf "$fmt" | "$BIN" hook claude-code "$@" 2>/dev/null | tr -d '\0')
    if [[ -n "$stdout" ]]; then
        echo "  ✗ $label: wrote to stdout: $stdout"
        fails=$((fails + 1))
    else
        echo "  ✓ $label"
    fi
}

if [[ ! -x "$BIN" ]]; then
    echo "release binary not found at $BIN — run: cargo build --release"
    exit 1
fi

echo "── release exit-0 guarantee (panic = abort) ────────"
export BECKON_HOME=/nonexistent/beckon/home

pipe_check "empty stdin"          ''
pipe_check "not json"             'garbage'
pipe_check "truncated json"       '{"hook_event_name":"Stop"'
pipe_check "json null"           'null'
pipe_check "json array"          '[]'
pipe_check "unknown event"       '{"hook_event_name":"NeverHeardOfIt"}'
# Bash command substitution strips NUL bytes, so this case pipes printf
# straight into the binary rather than going through "$(...)".
raw_check "binary stdin with NUL" '\xff\xfe\x00\x01'
pipe_check "hostile session id"  '{"session_id":"../../../../etc/passwd","cwd":"/tmp","hook_event_name":"Stop"}'

BECKON_PANIC_TEST=1 pipe_check "induced panic" '{}'

check "unknown agent"      0 empty "$BIN" hook no-such-agent

# A hook invocation must survive anything, including arguments a future agent
# release might add that this version has never seen.
raw_check "hook with an unknown flag" '{}' --extra
check "hook with an unknown flag, exit" 0 empty "$BIN" hook claude-code --some-future-flag

# The exit-0 guarantee is for the agent, not for people. A human mistyping a
# subcommand must get a code their shell can branch on.
check "unknown subcommand" 2 empty "$BIN" no-such-subcommand
check "no arguments"       2 empty "$BIN"
check "--help"             0 any   "$BIN" --help
check "--version"          0 any   "$BIN" --version

echo
if [[ $fails -gt 0 ]]; then
    echo "✗ $fails release safety check(s) failed"
    exit 1
fi
echo "✓ release safety verified"
