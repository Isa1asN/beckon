#!/usr/bin/env bash
# Everything CI enforces, in the order that fails fastest.
#
#   ./check.sh            fmt, clippy, tests
#   ./check.sh --release  also builds release and verifies the exit-0
#                         guarantee under panic = abort, which cargo test
#                         cannot reach
set -euo pipefail

echo "── fmt ────────────────────────────────────────────"
cargo fmt --check

echo "── clippy ─────────────────────────────────────────"
cargo clippy --all-targets --all-features -- -D warnings

echo "── test ───────────────────────────────────────────"
cargo test --all-features --quiet

# The shape a static musl build takes: no audio backend compiled in. CI runs
# this as its own job, so it belongs here too — feature-gated code is exactly
# where a test can quietly assume a decoder exists.
echo "── no audio backend ───────────────────────────────"
cargo clippy --all-targets --no-default-features --quiet -- -D warnings
cargo test --no-default-features --quiet

if [[ "${1:-}" == "--release" ]]; then
    echo "── release build ──────────────────────────────────"
    cargo build --release --quiet
    ./scripts/verify-release-safety.sh
fi

echo
echo "✓ all checks passed"
