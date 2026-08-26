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

if [[ "${1:-}" == "--release" ]]; then
    echo "── release build ──────────────────────────────────"
    cargo build --release --quiet
    ./scripts/verify-release-safety.sh
fi

echo
echo "✓ all checks passed"
