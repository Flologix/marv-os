#!/usr/bin/env bash
# Marv OS release: builds the CLI and the desktop with the kernel, smoke-tests them, and writes what a
# release on the public binaries-only repo Flologix/marv-os-releases holds. kernel-release.yml runs it in CI, started by hand; it runs the same locally.
#
#   scripts/marv-os-release.sh <version> [--publish]
#
#   <version>   semver without the tag's "os-v", e.g. 1.0.0-beta.1
#   --publish   also create the GitHub release os-v<version> on Flologix/marv-os-releases (a prerelease while the
#               version has a pre-release part). Without it nothing leaves this machine.
#
# Writes to $MARV_RELEASE_OUT (default target/os-release/<version>):
#   Marv_<version>_amd64.AppImage        the desktop, self-contained
#   marv-os_<version>_amd64.deb          the desktop for Debian/Ubuntu (package marv-os)
#   marv-<version>-linux-x86_64.tar.gz   the marv CLI, with the kernel
#   Marv_<version>_aarch64.dmg           the desktop for Apple silicon, unsigned (built by kernel-release.yml's
#                                        build-macos job, on macOS; copied in from $MARV_RELEASE_MACOS_DIR)
#   Marv_<version>_x64-setup.exe         the desktop for Windows x64, unsigned (built by windows.yml, called from
#                                        kernel-release.yml; copied in from $MARV_RELEASE_WINDOWS_DIR)
#   SHA256SUMS, RELEASE_NOTES.md
#
# The version is stamped into the desktop build only (tauri's --config override: the package and Settings > Activity >
# About say it); nothing is committed. `marv --version` keeps the engine's version (the workspace's).
#
# Env: RELEASES_TOKEN  a token for --publish (contents:write on marv-os-releases); else gh's own login.
#      MARV_RELEASE_MACOS_DIR  the folder holding Marv_<version>_aarch64.dmg (+ .sha256). --publish refuses without it;
#                              a dry run leaves the .dmg out.
#      MARV_RELEASE_WINDOWS_DIR  the folder holding Marv_<version>_x64-setup.exe (+ .sha256), the same way.
#      MARV_RELEASE_WHATS_NEW  a Markdown file for the notes' "What's new" instead of release-notes/os-v<version>.md.
#
# The notes go to a PUBLIC repo, so "What's new" is always written by hand, never made from commit subjects: the
# committed release-notes/os-v<version>.md (preferred: the tag's notes are reviewed in a PR) or MARV_RELEASE_WHATS_NEW.
# --publish refuses without one; a dry run uses a marked placeholder. Either way the file is refused if it has a
# private marker (see private_markers below).
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD

usage() { echo "usage: $0 <version> [--publish]" >&2; exit 2; }
[ $# -ge 1 ] || usage
version=$1; shift
publish=0
for a in "$@"; do
  case $a in --publish) publish=1 ;; *) usage ;; esac
done
# Semver (tauri wants it for the bundle version): MAJOR.MINOR.PATCH with an optional dotted pre-release.
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$ ]] || {
  echo "release: '$version' isn't a version like 1.0.0-beta.1" >&2; exit 2
}
tag="os-v$version"
releases_repo=Flologix/marv-os-releases

command -v mise >/dev/null && eval "$(mise env -s bash)" || true
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
export CARGO_TARGET_DIR="$target_dir"
out="${MARV_RELEASE_OUT:-$target_dir/os-release/$version}"
tmp=$(mktemp -d /tmp/marv-release.XXXX); trap 'rm -rf "$tmp"' EXIT
export TMPDIR="$tmp"
step() { printf '\n== %s\n' "$*"; }
fail() { echo "release: FAIL: $*" >&2; exit 1; }

use=$(df --output=pcent "$target_dir" 2>/dev/null || df --output=pcent "$root") && use=$(echo "$use" | tail -1 | tr -dc 0-9)
echo "disk use ${use}%"
[ "$use" -lt 89 ] || fail "disk above 88%: free space before building a release"

# The hand-written "What's new", checked before anything is built.
notes="${MARV_RELEASE_WHATS_NEW:-$root/release-notes/$tag.md}"
if [ -f "$notes" ]; then
  echo "notes: $notes"
elif [ -n "${MARV_RELEASE_WHATS_NEW:-}" ]; then
  fail "MARV_RELEASE_WHATS_NEW: $notes doesn't exist"
elif [ "$publish" = 1 ]; then
  fail "no release notes: write release-notes/$tag.md (and merge it) or set MARV_RELEASE_WHATS_NEW; they're public"
else
  notes=; echo "notes: none (release-notes/$tag.md); a dry run uses a placeholder"
fi
# What must never reach the public notes (case-insensitive, extended regex), one per line: the private plan and its
# sections, this repo's name, and bare issue refs (#27: this repo's issues; write Flologix/marv#123 for a public one).
private_markers='runtime-kernel
§
marv-kernel
\bplan\b
(^|[^/[:alnum:]])#[0-9]+'
if [ -n "$notes" ]; then
  hits=$(grep -inE -f <(printf '%s\n' "$private_markers") "$notes" || true)
  [ -z "$hits" ] || { printf '%s\n' "$hits" >&2; fail "$notes has private markers (above): rewrite those lines"; }
fi

engine=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
appimage="Marv_${version}_amd64.AppImage"
deb="marv-os_${version}_amd64.deb"
cli="marv-${version}-linux-x86_64.tar.gz"
dmg="Marv_${version}_aarch64.dmg"
exe="Marv_${version}_x64-setup.exe"
rm -rf "$out"; mkdir -p "$out"

# The macOS .dmg comes from the hosted macOS job; a release never goes out without it.
macos_dir="${MARV_RELEASE_MACOS_DIR:-}"
if [ -n "$macos_dir" ] && [ -f "$macos_dir/$dmg" ]; then
  if [ -f "$macos_dir/$dmg.sha256" ]; then
    want=$(cut -d' ' -f1 "$macos_dir/$dmg.sha256"); got=$(sha256sum "$macos_dir/$dmg" | cut -d' ' -f1)
    [ "$want" = "$got" ] || fail "$dmg doesn't match its .sha256"
  fi
  cp "$macos_dir/$dmg" "$out/$dmg"; echo "macOS: $dmg"
elif [ "$publish" = 1 ]; then
  fail "no $dmg in MARV_RELEASE_MACOS_DIR='$macos_dir': the macOS build must finish first"
else
  dmg=; echo "macOS: no .dmg (MARV_RELEASE_MACOS_DIR); a dry run leaves it out"
fi

# The Windows installer comes from the hosted Windows job, the same way; a release never goes out without it either.
windows_dir="${MARV_RELEASE_WINDOWS_DIR:-}"
if [ -n "$windows_dir" ] && [ -f "$windows_dir/$exe" ]; then
  if [ -f "$windows_dir/$exe.sha256" ]; then
    want=$(cut -d' ' -f1 "$windows_dir/$exe.sha256"); got=$(sha256sum "$windows_dir/$exe" | cut -d' ' -f1)
    [ "$want" = "$got" ] || fail "$exe doesn't match its .sha256"
  fi
  cp "$windows_dir/$exe" "$out/$exe"; echo "Windows: $exe"
elif [ "$publish" = 1 ]; then
  fail "no $exe in MARV_RELEASE_WINDOWS_DIR='$windows_dir': the Windows build must finish first"
else
  exe=; echo "Windows: no installer (MARV_RELEASE_WINDOWS_DIR); a dry run leaves it out"
fi

# --- build ---
step "build marv (release)"
cargo build --release -p marv-cli
marv="$target_dir/release/marv"

step "build the desktop $version (AppImage, deb)"
# Both bundles ship this marv at usr/libexec/marv-os/marv (app/src-tauri/tauri.linux.conf.json): the desktop runs its
# sandboxed helpers (__relay, __run-checks, __html-convert) with it, since the sandbox can't see the AppImage's bundled
# WebKitGTK that marv-desktop itself needs (mk-ssf).
install -D -m 755 "$marv" app/src-tauri/binaries/marv
bundle="$target_dir/release/bundle"
rm -rf "$bundle/appimage" "$bundle/deb"
# APPIMAGE_EXTRACT_AND_RUN: tauri's linuxdeploy is itself an AppImage; this runs it without FUSE 2.
# NO_STRIP: linuxdeploy's bundled strip is older than this toolchain's ELF sections (.relr.dyn) and fails on them;
# cargo's release profile strips the binary already.
(cd app && npm ci --no-audit --no-fund &&
  APPIMAGE_EXTRACT_AND_RUN=1 NO_STRIP=true npx tauri build --bundles appimage,deb --config "{\"version\":\"$version\"}")
desktop="$target_dir/release/marv-desktop"
built_appimage=$(ls "$bundle"/appimage/*.AppImage 2>/dev/null | head -1)
built_deb=$(ls "$bundle"/deb/*.deb 2>/dev/null | head -1)
[ -n "$built_appimage" ] || fail "tauri made no AppImage in $bundle/appimage"
[ -n "$built_deb" ] || fail "tauri made no .deb in $bundle/deb"

# --- package ---
step "package"
cp "$built_appimage" "$out/$appimage"; chmod 755 "$out/$appimage"

# The .deb, repacked with the package name marv-os (tauri names it after productName, "marv") and a Debian version
# (1.0.0~beta.1 sorts before 1.0.0; 1.0.0-beta.1 would read as revision "beta.1" of 1.0.0 and sort after it).
deb_version=${version/-/\~}
d="$tmp/deb"; mkdir -p "$d/control"
(cd "$d" && ar x "$built_deb")
control_tar=$(cd "$d" && ls control.tar.*); data_tar=$(cd "$d" && ls data.tar.*)
tar -C "$d/control" -xf "$d/$control_tar"
# And what tauri can't set: the maintainer (it writes the login's name), a long description (it writes "(none)"),
# the homepage, and bubblewrap, which the sandbox runs commands in.
sed -i -e 's/^Package: .*/Package: marv-os/' -e "s/^Version: .*/Version: $deb_version/" \
  -e 's/^Maintainer: .*/Maintainer: FloLogix AI/' -e '/^ (none)$/d' "$d/control/control"
sed -i -e '/^Depends:/a Recommends: bubblewrap' -e '/^Priority:/a Homepage: https://github.com/Flologix/marv-os-releases' \
  "$d/control/control"
sed -i -e 's/^Description: .*/Description: Marv OS: run several coding agents on your own machine/' "$d/control/control"
cat >>"$d/control/control" <<'EOF2'
 The Marv desktop by FloLogix AI: see what each coding agent is doing, what
 needs you, and what happened while you were away. Built on the open-source
 Marv engine.
EOF2
(cd "$d/control" && tar --sort=name --owner=0 --group=0 --numeric-owner -czf "$d/control.tar.gz" ./*)
[ "$control_tar" = control.tar.gz ] || rm -f "$d/$control_tar"
(cd "$d" && rm -f "$out/$deb" && ar rcD "$out/$deb" debian-binary control.tar.gz "$data_tar")

# The CLI: marv at the archive's root, packed the same bytes every time (as packaging/package.sh does).
stage="$tmp/cli"; mkdir -p "$stage"
install -m 755 "$marv" "$stage/marv"
tar -C "$stage" --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -cf - marv | gzip -n -9 >"$out/$cli"

# --- smoke tests (never the network, never a real model) ---
step "smoke: marv"
v=$("$marv" --version) || fail "marv --version failed"
case $v in marv\ *) echo "marv --version: $v" ;; *) fail "marv --version said '$v'" ;; esac
sandbox="$tmp/home"; mkdir -p "$sandbox/.config" "$sandbox/run"; chmod 700 "$sandbox/run"
about=$(env -i PATH="$PATH" HOME="$sandbox" MARV_CONFIG_DIR="$sandbox/.marv" XDG_RUNTIME_DIR="$sandbox/run" \
  "$marv" kernel about) || fail "marv kernel about failed"
printf '%s\n' "$about" | sed 's/^/  | /'
printf '%s\n' "$about" | head -1 | grep -q '^Marv kernel ' || fail "marv kernel about didn't name the kernel"
tar -tzf "$out/$cli" | grep -qx marv || fail "$cli doesn't hold marv"

step "smoke: a sandboxed marv -p turn (packaging/smoke.sh, its stub provider)"
# The public release smoke test on this binary: install.sh from a packed asset, then a turn against the local stub
# provider that must stay inside bubblewrap. The asset name is only what install.sh looks for.
packaging/package.sh x86_64-unknown-linux-musl "$marv" "$tmp/assets" >/dev/null
packaging/smoke.sh x86_64-unknown-linux-musl "$tmp/assets"

step "smoke: the .deb"
x="$tmp/deb-check"; mkdir -p "$x"
(cd "$x" && ar x "$out/$deb")
[ "$(cd "$x" && ar t "$out/$deb" | head -1)" = debian-binary ] || fail "$deb doesn't start with debian-binary"
tar -C "$x" -xzf "$x/control.tar.gz" ./control
sed 's/^/  | /' "$x/control"
grep -qx 'Package: marv-os' "$x/control" || fail "$deb's package isn't marv-os"
grep -qx "Version: $deb_version" "$x/control" || fail "$deb's version isn't $deb_version"
grep -qx 'Architecture: amd64' "$x/control" || fail "$deb isn't amd64"
grep -qx 'Depends: .*libwebkit2gtk-4.1-0.*' "$x/control" || fail "$deb doesn't depend on WebKitGTK 4.1"
files=$(tar -tf "$x"/data.tar.* | sed 's|^\./||')
for f in usr/bin/marv-desktop usr/libexec/marv-os/marv usr/share/applications/Marv.desktop; do
  printf '%s\n' "$files" | grep -qx "$f" || fail "$deb lacks $f (it has: $(printf '%s ' $files))"
done
echo "deb: $(printf '%s\n' "$files" | grep -vc '/$') files, /usr/bin/marv-desktop and its launcher entry"

step "smoke: the desktop"
missing=$(ldd "$desktop" | grep 'not found' || true)
[ -z "$missing" ] || fail "marv-desktop is missing libraries: $missing"
# (Its version can't be grepped from the binary: semver keeps a short pre-release inline as a number. The bundles'
# names and the .deb's Version come from the same --config, which the app's package info is built from.)
echo "marv-desktop: links"
(cd "$tmp" && "$out/$appimage" --appimage-extract >/dev/null) || fail "the AppImage didn't extract"
[ -x "$tmp/squashfs-root/usr/bin/marv-desktop" ] || fail "the AppImage lacks usr/bin/marv-desktop"
cmp -s "$tmp/squashfs-root/usr/bin/marv-desktop" "$desktop" || echo "note: the AppImage's marv-desktop differs from the build's (linuxdeploy patched it)"
helper="$tmp/squashfs-root/usr/libexec/marv-os/marv"
[ -x "$helper" ] || fail "the AppImage lacks usr/libexec/marv-os/marv (the sandboxed helpers' binary)"
cmp -s "$helper" "$marv" || fail "the AppImage's usr/libexec/marv-os/marv isn't this build's marv"
! ldd "$helper" | grep -Eq 'gtk|webkit' || fail "usr/libexec/marv-os/marv links GTK/WebKit: the sandbox can't see those"
out_relay=$(env -i "$helper" __relay '{"argv":["sh","-c","echo relayed; exit 7"]}') && code=0 || code=$?
[ "$code" = 7 ] && [ "$out_relay" = relayed ] || fail "the AppImage's helper didn't relay (exit $code: $out_relay)"
echo "AppImage: extracts, holds usr/bin/marv-desktop and the helper usr/libexec/marv-os/marv (relays, no GTK)"
# A real start needs a display: under xvfb-run when it's here, it must still be running after 15 s.
if command -v xvfb-run >/dev/null; then
  set +e
  env -i PATH="$PATH" HOME="$sandbox" XDG_RUNTIME_DIR="$sandbox/run" APPIMAGE_EXTRACT_AND_RUN=1 \
    xvfb-run -a timeout 15 "$out/$appimage" >"$tmp/gui.log" 2>&1
  code=$?
  set -e
  [ "$code" -eq 124 ] || { sed 's/^/  | /' "$tmp/gui.log"; fail "the AppImage exited $code within 15 s under xvfb"; }
  echo "AppImage: started under xvfb and was still running after 15 s"
else
  echo "AppImage: no xvfb-run here, so it wasn't started (install xorg-server-xvfb / xvfb for a launch test)"
fi

# --- checksums and notes ---
step "SHA256SUMS, RELEASE_NOTES.md"
(cd "$out" && sha256sum "$appimage" "$deb" "$cli" ${dmg:+"$dmg"} ${exe:+"$exe"} >SHA256SUMS && cat SHA256SUMS)

if [ -n "$dmg" ]; then macos_line="Open \`$dmg\` and drag Marv to Applications."
else macos_line="The macOS download isn't part of this build."; fi
# A pre-release (1.0.0-beta.2) says so; a stable release doesn't.
case $version in *-*) beta_line="- Pre-release: expect rough edges, and keep backups of work you care about.
" ;; *) beta_line= ;; esac
if [ -n "$exe" ]; then windows_line="Run \`$exe\` and follow the installer."
else windows_line="The Windows download isn't part of this build."; fi
{
  echo "# Marv OS $version"
  echo
  echo "Released $(date -u +%Y-%m-%d). Built on the open-source Marv engine $engine."
  echo
  echo "## What's new"
  echo
  if [ -n "$notes" ]; then
    cat "$notes"
  else
    echo "> **PLACEHOLDER (dry run):** no release-notes/$tag.md. Publishing refuses until it's written."
  fi
  cat <<EOF

## Install (Linux x86_64)

- **AppImage:** \`chmod +x $appimage && ./$appimage\`
- **Debian/Ubuntu:** \`sudo apt install ./$deb\`
- **CLI only:** \`tar -xzf $cli && install -m 755 marv ~/.local/bin/marv\`

## Install (macOS, Apple silicon)

$macos_line

The app isn't signed yet, so macOS won't open it straight away: drag Marv to Applications first and open it. When macOS
says it could not verify the app, go to System Settings > Privacy & Security and click Open Anyway (or run
\`xattr -dr com.apple.quarantine /Applications/Marv.app\`). After that it opens normally.

## Install (Windows x64)

$windows_line

The installer isn't signed yet, so Windows SmartScreen asks before the first run: choose More info, then Run anyway.
Marv OS on Windows has no command sandbox yet: agents and commands reach what your user account can, and each change
is asked first. Git for Windows is needed for the Git tab, turns and worktrees (Marv offers to install it).

Check the downloads first: \`sha256sum -c SHA256SUMS --ignore-missing\`.

You'll need a model: Marv's Pro + models plan (models included, no key), your own OpenRouter key, a ChatGPT Plus or Pro
account, a Kimi Code membership, or Ollama running locally with a model that supports tool calling. Coding agents you
already use (Claude Code, Codex, OpenCode) run on their own logins. The first run walks you through it.

## Known limits

${beta_line}- Native builds: Linux x86_64, macOS on Apple silicon (Intel Macs aren't covered) and Windows x64.
- macOS and Windows builds are unsigned, so each asks once before the first launch (see above).
- Commands run in a sandbox (bubblewrap): install \`bwrap\` if your distribution doesn't ship it.
- Installing the open-source Marv (npm or its install script) replaces this \`marv\` CLI with the engine alone.
EOF
} >"$out/RELEASE_NOTES.md"
sed 's/^/  | /' "$out/RELEASE_NOTES.md"

step "artifacts in $out"
ls -lh "$out"

if [ "$publish" = 0 ]; then
  step "dry run: nothing published (pass --publish to release $tag on $releases_repo)"
  exit 0
fi

step "publish $tag to $releases_repo"
[ -z "${RELEASES_TOKEN:-}" ] || export GH_TOKEN="$RELEASES_TOKEN"
if gh release view "$tag" -R "$releases_repo" >/dev/null 2>&1; then
  fail "$releases_repo already has a release $tag"
fi
pre=(); case $version in *-*) pre=(--prerelease) ;; esac
(cd "$out" && gh release create "$tag" -R "$releases_repo" "${pre[@]}" --title "Marv OS $version" \
  --notes-file RELEASE_NOTES.md "$appimage" "$deb" "$cli" ${dmg:+"$dmg"} ${exe:+"$exe"} SHA256SUMS)
step "published: https://github.com/$releases_repo/releases/tag/$tag"
