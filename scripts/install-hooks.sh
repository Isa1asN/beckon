#!/usr/bin/env bash
# Install the repo's git hooks. Run once after cloning.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
install -m 0755 "$root/scripts/pre-commit" "$root/.git/hooks/pre-commit"
echo "✓ installed pre-commit hook (fmt + clippy)"
