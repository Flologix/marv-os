Pasting into an agent's terminal no longer freezes Marv OS.

### Fixed

- **Pasting into a terminal could freeze the whole window** on Linux (Wayland): text copied in Marv OS itself and
  pasted with ctrl+shift+v or a right-click left the window waiting on itself. The paste now reads the clipboard off
  the window's own thread, and gives up after 5 seconds instead of waiting forever.

### Known issues

- **macOS and Windows builds aren't signed yet**, so each asks once before the first open.

### Feedback

Please report anything that breaks, feels slow or confuses you in the
[issues](https://github.com/Flologix/marv-os/issues).
