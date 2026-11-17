#!/usr/bin/env bash
# Type-checks the workspace for Windows from Linux: `scripts/windows/check.sh [cargo check args]`.
# Needs zig (C compiler for ring and friends), llvm-windres (the desktop's icon resource) and
# `rustup target add x86_64-pc-windows-gnu`. Builds into $CARGO_TARGET_DIR-win so the Linux build isn't touched.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# A real zig: $ZIG, else one mise installed (its shim needs a version set in this folder), else zig on PATH.
zig=${ZIG:-$(ls -d "$HOME"/.local/share/mise/installs/zig/*/zig 2>/dev/null | sort -V | tail -1)}
zig=${zig:-$(command -v zig)}
tools=$(mktemp -d)
trap 'rm -rf "$tools"' EXIT
# zig names the target x86_64-windows-gnu; drop the --target cc-rs adds in Rust's spelling.
# Not named cc: on PATH that would also link the host build scripts as Windows programs.
cat > "$tools/zig-win-cc" <<SH
#!/bin/sh
for a do shift; case "\$a" in --target=*|-target) ;; *) set -- "\$@" "\$a";; esac; done
exec "$zig" cc -target x86_64-windows-gnu "\$@"
SH
printf '#!/bin/sh\nexec "%s" ar "$@"\n' "$zig" > "$tools/zig-win-ar"
printf '#!/bin/sh\nexec llvm-windres "$@"\n' > "$tools/x86_64-w64-mingw32-windres"
chmod +x "$tools"/*
export PATH="$tools:$PATH" CC_x86_64_pc_windows_gnu="$tools/zig-win-cc" AR_x86_64_pc_windows_gnu="$tools/zig-win-ar"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$here/../../target}-win"
cd "$here/../.."
# The Windows bundle config names binaries/marv.exe and the bundled ConPTY (the release copies the real ones in; the
# folder is ignored): a check only needs the paths to exist.
mkdir -p app/src-tauri/binaries
for f in marv.exe conpty.dll OpenConsole.exe; do [ -e "app/src-tauri/binaries/$f" ] || : >"app/src-tauri/binaries/$f"; done
cargo check --target x86_64-pc-windows-gnu --workspace "$@"
