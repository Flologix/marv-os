/** First-run choices kept in this window's storage. */
export const SKIPPED_KEY = "marv-brain-skipped";

/** The person skipped the brain step (they only want Claude Code or Codex): no provider is asked for again. */
export function brainSkipped(): boolean {
  try {
    return localStorage.getItem(SKIPPED_KEY) === "1";
  } catch {
    return false;
  }
}
