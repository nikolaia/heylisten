#!/usr/bin/env bash
# What CI checks, run locally: the tests, clippy, and known vulnerabilities in the dependencies
# (cargo audit, against today's advisory database). The pre-push hook runs this before every push.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo test --locked --release
cargo clippy --locked --release --all-targets --features tray -- -D warnings -A linker_messages
cargo audit
echo "All checks passed."
