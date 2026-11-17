/** The agents Marv OS can run chats with besides Marv (the harness plan): their record names and titles. */
export const HARNESS_TITLES: Record<string, string> = {
  marv: "Marv",
  "claude-code": "Claude Code",
  codex: "Codex",
  opencode: "OpenCode",
  gemini: "Gemini CLI",
  pi: "Pi",
  "cursor-agent": "Cursor Agent",
  crush: "Crush",
  antigravity: "Antigravity",
};

/** What the page calls the agent that runs a chat ("Marv" for none). */
export function harnessTitle(id: string | null | undefined): string {
  if (!id) return "Marv";
  return HARNESS_TITLES[id] ?? id;
}

/** How to get an agent that isn't installed yet. */
export const HARNESS_INSTALL: Record<string, string> = {
  "claude-code": "get it from claude.com/claude-code, then sign in once with claude in a terminal",
  opencode: "get it from opencode.ai; it runs free models with no sign-in, or sign in with opencode auth login",
  gemini: "npm i -g @google/gemini-cli, then sign in once with gemini in a terminal",
  antigravity: "get the Antigravity CLI from antigravity.google, then sign in once with agy in a terminal",
};

/** How much a chat's agent may do without asking (marv_web::terminal_agents::Permissions), the same four for every
 * agent, set on each its own way; "own": the agent's own settings. */
export const PERMISSION_MODES: { id: string; name: string; text: string }[] = [
  { id: "own", name: "Its own settings", text: "as in your terminal: whatever the agent is set to" },
  { id: "supervised", name: "Supervised", text: "asks before each edit and command" },
  { id: "auto-edits", name: "Auto-accept edits", text: "edits in this folder go ahead; commands ask" },
  { id: "auto", name: "Auto", text: "the agent's own review approves routine work (Claude Code's auto mode; others: auto-accept edits)" },
  { id: "full", name: "Full access", text: "nothing asks, so it runs inside Marv's sandbox: this folder and the agent's own settings only" },
];

/** The modes offered here: Full access only where Marv's sandbox can run (`AppInfo.sandbox`), as it's refused
 * everywhere else (`terminal_agents::command`). */
export function permissionModes(sandbox: boolean): typeof PERMISSION_MODES {
  return sandbox ? PERMISSION_MODES : PERMISSION_MODES.filter((m) => m.id !== "full");
}

export function permissionName(id: string | null | undefined): string {
  return PERMISSION_MODES.find((m) => m.id === (id ?? "own"))?.name ?? "Its own settings";
}
