#!/usr/bin/env bash
# A screenshot of each place of Marv desktop's page (Home, Folders with each dock tab, Teams, Activity,
# Apps, Settings) in headless Chromium, desktop_mock.js standing in for the Tauri backend (demo folders and chats under
# /home/demo, no keys). Nothing runs behind it. The default out-dir is under target/.
#
#   scripts/screenshots/capture-places.sh [out-dir]
#
# Needs: npm (app/'s node_modules installed: `cd app && npm ci`), python3, chromium, magick (ImageMagick 7).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
app=$here/../../app
out=${1:-$here/../../target/screenshots}
mkdir -p "$out"
out=$(realpath "$out")
port=${SHOT_PORT:-8767}
tmp=$(mktemp -d "${TMPDIR:-/tmp}/marv-places-shot.XXXXXX")
server_pid=
cleanup() {
  [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT

(cd "$app" && npx vite build --outDir "$tmp/dist" --emptyOutDir --logLevel error)
cp "$here/desktop_mock.js" "$tmp/dist/"
sed -i 's|<head>|<head><script src="/desktop_mock.js"></script>|' "$tmp/dist/index.html"
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$tmp/dist" >/dev/null 2>&1 &
server_pid=$!
sleep 0.5

shot() { # name query
  "${CHROMIUM:-chromium}" --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=2 --window-size=1440,900 \
    --user-data-dir="$tmp/profile-$1" --virtual-time-budget=4000 --screenshot="$tmp/$1.raw.png" "http://127.0.0.1:$port/?$2" 2>/dev/null
  magick "$tmp/$1.raw.png" -resize 50% -strip -define png:compression-level=9 "$out/$1.png"
  echo "$out/$1.png"
}

shot home "place=home"
for tab in files changes board terminal context; do shot "folders-$tab" "place=folders&dock=$tab"; done
shot teams "place=teams"
shot activity "place=activity"
shot apps "place=apps"
shot settings "place=settings"
shot folders-dark "place=folders&dock=changes&theme=dark"
# One-step team setup: the New team dialog and a started team's Team view.
shot team-new "place=teams&team=new"
shot team-view "place=teams&team=view"
shot team-sidebar "place=folders&team=view"
