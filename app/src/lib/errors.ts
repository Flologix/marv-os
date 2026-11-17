/**
 * Kernel #41: errors the person shouldn't see in a dialog. When the watchdog reloads the page, or its web process
 * ends, calls in flight fail with the link to the app (a fetch to the IPC protocol), and the page is going away: an
 * error box then is noise at best, and a native dialog opened while the window rebuilds its page has crashed it.
 * Such errors are logged instead.
 */

/** The link to the app broke (the page is reloading, the web process ended), not the call itself. */
export function isTransportError(e: unknown): boolean {
  const text = e instanceof Error ? `${e.name}: ${e.message}` : String(e);
  return /Failed to fetch|Load failed|NetworkError when attempting to fetch|IPC custom protocol failed|ipc\.localhost/i.test(text);
}

/** Whether `e` may be shown in a dialog: never while the page unloads, never for a broken link to the app. */
export function shouldSay(e: unknown, unloading: boolean): boolean {
  return !unloading && !isTransportError(e);
}
