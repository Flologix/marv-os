#!/usr/bin/env bash
# Marv OS: every check a change must pass, the same locally and in CI (ci.yml).
#   scripts/marv-os-check.sh           everything
#   scripts/marv-os-check.sh --quick   fmt, clippy and the Rust tests only (no oracle, no desktop page)
# Uses a TMPDIR under /tmp (Unix socket paths have a 107-byte limit) and removes it after.
set -euo pipefail
cd "$(dirname "$0")/.."
quick=0; [ "${1:-}" = "--quick" ] && quick=1
command -v mise >/dev/null && eval "$(mise env -s bash)" || true
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}"
tmp=$(mktemp -d /tmp/marv-check.XXXX); trap 'rm -rf "$tmp"' EXIT
# The compiler cache server (sccache, the user-wide rustc-wrapper here) is shared by every build on the machine and
# keeps the TMPDIR of whoever starts it. Start it before switching to our TMPDIR, which is removed on exit: a server
# started under it breaks every later build ("sccache: Failed to create temp dir"), such as kernel-release.yml's.
command -v sccache >/dev/null 2>&1 && { sccache --start-server >/dev/null 2>&1 || true; }
export TMPDIR="$tmp"
# The guard tests run the built marv; point them at it wherever CARGO_TARGET_DIR puts it (kernel-release.yml
# runs --quick without setting this, and its target dir is outside the checkout).
export MARV_ORACLE_BIN="${MARV_ORACLE_BIN:-${CARGO_TARGET_DIR:-$PWD/target}/debug/marv}"
step() { printf '\n== %s\n' "$*"; }

step "fmt";    cargo fmt --all --check
step "clippy"; cargo clippy --workspace --all-targets -- -D warnings
step "build";  cargo build -p marv-cli
before=$(find /tmp -maxdepth 1 -ignore_readdir_race \( -name 'marv-*' -o -name 'mx-*' \) ! -name "marv-$(id -u)" ! -name "$(basename "$tmp")" | wc -l)
step "tests";  cargo test --workspace --no-fail-fast
after=$(find /tmp -maxdepth 1 -ignore_readdir_race \( -name 'marv-*' -o -name 'mx-*' \) ! -name "marv-$(id -u)" ! -name "$(basename "$tmp")" | wc -l)
# Marv's own runtime folder (marv-<uid>, used where there's no XDG_RUNTIME_DIR) is expected; anything else is a leak.
leftovers=$(find "$tmp" -mindepth 1 -maxdepth 1 ! -name "marv-$(id -u)")
if [ "$after" -gt "$before" ] || [ -n "$leftovers" ]; then
  echo "the tests left temp folders behind ($((after - before)) in /tmp):"; printf '%s\n' "$leftovers"; exit 1
fi
if [ "$quick" = 0 ]; then
  step "oracle"; cargo test -p marv-oracle -- --ignored
  step "desktop page"; (cd app && npm ci --no-audit --no-fund && npx tsc -b && npm test && npm run build)
fi
step "all checks passed"
