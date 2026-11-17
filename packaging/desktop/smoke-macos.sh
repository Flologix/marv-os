#!/bin/sh
# Launches a built Marv.app's binary and waits for it to say it's up (#208): the window opened, the bundled page ran
# and asked the backend for the launch folder (`MARV_DESKTOP_SMOKE_TEST=1`, see take_launch in app/src-tauri/src/lib.rs).
# Fails if it doesn't say so within the time limit, or exits without it.
#
#   packaging/desktop/smoke-macos.sh path/to/Marv.app
set -eu
app=$1
limit=${MARV_DESKTOP_SMOKE_SECONDS:-120}
# The app's own binary by the name in Info.plist: Contents/MacOS also holds lazygit.
exe=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist" 2>/dev/null || true)
bin="$app/Contents/MacOS/$exe"
[ -n "$exe" ] && [ -x "$bin" ] || { echo "no binary named by $app/Contents/Info.plist in $app/Contents/MacOS"; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/marv-desktop-smoke.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
mkdir -p "$work/home" "$work/folder"
log="$work/log"

# One launch, waiting for `ready`: `smoke_run <label> <env command...>`, the command being what starts the binary (so the
# second pass can clear the environment).
smoke_run() {
  label=$1
  shift
  log="$work/log-$label"
  : >"$log"
  "$@" "$bin" "$work/folder" >"$log" 2>&1 &
  pid=$!
  waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt "$limit" ]; do
    grep -q '^marv-desktop: ready' "$log" && break
    sleep 1
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    # Asked to quit after printing: give it a moment, then stop it.
    sleep 5
    kill "$pid" 2>/dev/null || true
  fi
  wait "$pid" 2>/dev/null || true
  echo "== $label: $bin output"
  cat "$log"
  grep -q '^marv-desktop: ready' "$log" || { echo "Marv desktop ($label) didn't reach ready within ${limit}s"; exit 1; }
  echo "Marv desktop ($label) reached ready in ${waited}s"
}

# A HOME of its own, so the app's settings and chats land in the temp folder; the launch folder is outside it (the
# Marv folder can't be HOME or above it).
smoke_run full env HOME="$work/home" MARV_DESKTOP_SMOKE_TEST=1

# Again as Finder starts it: the bare launchd environment (no LANG, PATH of /usr/bin:/bin:/usr/sbin:/sbin), a fresh HOME
# so nothing from the first pass is reused. The app re-runs itself with the login shell's PATH (login_env.rs) and must
# still reach ready; a login shell with no rc files in the temp HOME just leaves PATH as it is.
mkdir -p "$work/home2"
smoke_run finder env -i HOME="$work/home2" PATH=/usr/bin:/bin:/usr/sbin:/sbin MARV_DESKTOP_SMOKE_TEST=1
