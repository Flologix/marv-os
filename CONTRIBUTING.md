# Contributing to Marv OS

Thanks for helping. This file covers how to build, test and send a change. By contributing you agree that your
contribution is licensed under the Apache License 2.0, as Section 5 of the license says. There is no CLA to sign.

Please read [AGENTS.md](AGENTS.md) too: it holds the project's code rules and is the file coding agents read here.

## Build

You need the Rust toolchain (`rust-toolchain.toml` selects stable with clippy and rustfmt) and, for the desktop app,
Node.js 22 and npm.

```sh
cargo build -p marv-cli          # the marv command
cd app && npm ci                 # the desktop app's web page
```

On Debian or Ubuntu the desktop and the sandbox tests need:
`sudo apt-get install bubblewrap libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`.
On Ubuntu 24.04 also allow unprivileged user namespaces for bubblewrap:
`sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`.

## Test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p <crate>            # while working on one crate
cargo test --workspace --no-fail-fast
(cd app && npx tsc -b && npm test && npm run lint)
```

`scripts/marv-os-check.sh` runs the whole set (add `--quick` to skip the oracle and the desktop page). CI runs the
same checks on every pull request.

The sandbox guards in `tests/guards/` skip when bubblewrap cannot start a sandbox. Set `MARV_REQUIRE_SANDBOX=1` to make
that a failure instead, so a green run means they ran.

## Code rules

- No `unsafe`, apart from the two audited functions named in AGENTS.md. `unsafe_code` is `deny`, and a test keeps those
  the only ones.
- Every security rule in [docs/SECURITY.md](docs/SECURITY.md) has a guard test in `tests/guards/` that fails when the
  rule is removed. A change that adds or relaxes a rule needs a guard test, and a note in that file.
- Comments explain why, not what: the bug that was hit, the constraint, the surprise. Keep the existing ones when you
  move code.
- Model or tool text never reaches the terminal without going through `printable()`.
- Keep the prompt cache stable: history is append-only, and the system prompt is byte-stable within a session.
- Done means `cargo check` and `cargo test` pass with no new warnings, and the docs under `docs/subsystems/` that
  describe what you changed are updated.

## Pull requests

- Keep a pull request to one change, and say what it does and why.
- Add or update tests for behavior you change. List the checks you ran in the description.
- Add a screenshot for a change to the desktop's looks.
- Add a line to the `[Unreleased]` section of CHANGELOG.md for anything a user would notice.
- Report security problems privately, as described in [SECURITY.md](SECURITY.md), not in a pull request.
