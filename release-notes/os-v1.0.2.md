Pasting into an agent's terminal no longer freezes Marv OS, and signing in to Gmail works, in Marv's chats and in Claude Code's.

### Fixed

- **Pasting into a terminal could freeze the whole window** on Linux (Wayland): text copied in Marv OS itself and
  pasted with ctrl+shift+v or a right-click left the window waiting on itself. The paste now reads the clipboard off
  the window's own thread, and gives up after 5 seconds instead of waiting forever.
- **Signing in to Gmail said "signed in" without signing in**: no browser opened, nothing was saved, and Marv's
  chats then said Gmail needed a new login. Gmail's server lists its tools before you sign in, which Marv took for
  "no sign-in needed". Sign in now opens Google's page whenever the app offers a sign-in.
- **Gmail in a Claude Code chat said "client_secret is missing"**: Claude Code now gets the app's client secret in
  its own secret store (never in the chat's files), so `/mcp` signs in to Gmail there too.

### Known issues

- **macOS and Windows builds aren't signed yet**, so each asks once before the first open.

### Feedback

Please report anything that breaks, feels slow or confuses you in the
[issues](https://github.com/Flologix/marv-os/issues).
