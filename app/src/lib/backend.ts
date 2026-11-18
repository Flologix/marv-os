// What the page talks to: Marv desktop's Tauri commands, or marv-web's WebSocket in a plain browser. A chat looks
// the same either way: messages out (send, interrupt, approval, configure), the same JSON events back.
import { getVersion } from "@tauri-apps/api/app";
import { Channel, invoke as call } from "@tauri-apps/api/core";

/** A line for the app's log (Settings › About › Logs): an error on the page, a call that failed. Never throws. */
export function logLine(level: "info" | "warn" | "error", area: string, text: string) {
  void call("log_page", { level, area, text: text.slice(0, 2000) }).catch(() => {});
}

/** The same failure said once per 30 s, so a call retried in a loop doesn't fill the log. */
const lastLogged = new Map<string, number>();

/** Every call to the app, its failure in the app's log (`page/<command>`): many are only shown briefly, or not at all
 * where a terminal covers the transcript, so a report had nothing to go on. */
function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return call<T>(command, args).catch((e: unknown) => {
    const text = String(e);
    const key = `${command}:${text}`;
    const now = Date.now();
    if ((lastLogged.get(key) ?? 0) + 30_000 < now) {
      lastLogged.set(key, now);
      logLine("warn", command, text);
    }
    throw e;
  });
}
import type { PlanStatus } from "./plan";

export const isDesktop = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export interface Space {
  id: string;
  name: string;
  root: string;
  /** Apps (MCP servers) switched off in this space. */
  appsOff: string[];
}

export interface AppInfo {
  root: string | null;
  rootName: string | null;
  hasKey: boolean;
  /** "openrouter", "ollama", "zai", "kimi" or "chatgpt", once chosen at onboarding. */
  provider: string | null;
  /** The chosen provider has what it needs (a key, a sign-in). */
  ready: boolean;
  /** Onboarding offers the Z.ai Coding Plan / the Kimi Code plan / Sign in with ChatGPT (on by default). */
  offerZai: boolean;
  offerKimi: boolean;
  offerChatgpt: boolean;
  hasZaiKey: boolean;
  hasKimiKey: boolean;
  /** Who is signed in with ChatGPT, if anyone. */
  chatgptAccount: string | null;
  spaces: Space[];
  active: string | null;
  /** The home folder (real path), where Home chats run. */
  home: string;
  /** Signed in to a Marv account here, free or paid. */
  signedIn: boolean;
  /** The first run's "Sign in to Marv" step was answered (signed in or skipped). */
  accountAsked: boolean;
  /** Marv's outer sandbox can run here (Linux with bubblewrap; not on Windows yet). */
  sandbox: boolean;
}

/** A model of a plan set up here: `zai/<model>`, `kimi/<model>` or `chatgpt/<slug>`. */
export interface PlanModel {
  id: string;
  plan: string;
  context: number | null;
  /** What to show (ChatGPT's display name), when the list has one. */
  name: string | null;
}

export interface LocalModel {
  id: string;
  tools: boolean;
}

export interface Entry {
  name: string;
  dir: boolean;
}

/** One of your mesh rules, as the Mesh panel shows it. */
export interface MeshRule {
  id: number;
  label: string;
  from: string;
  to: string;
  mode: string;
  mutual: boolean;
  expiresAtMs: number | null;
  untilQuit: boolean;
  by: string;
}

export interface MeshRules {
  rules: MeshRule[];
  /** Why your rules file isn't used, if it isn't (it then counts as no rules). */
  problem: string | null;
  /** What the click did beyond the rule (chats moved to their own worktrees). */
  notice?: string;
}

/** One entry on a rule's team board (runtime-kernel §19): `by` a chat's mesh name, or empty for you. */
export interface BoardEntry {
  /** Where it is kept: [rule, entry id] (your post to a team's boards is kept in each, shown once). */
  refs: [number, number][];
  by: string;
  kind: "claim" | "note" | "done";
  text: string;
  paths: string[];
  atMs: number;
}

export interface Board {
  rules: number[];
  entries: BoardEntry[];
  /** Why the board can't be read, if it can't. */
  problem: string | null;
}

/** A chat's grant to keep working while you're away: what yolo would run, nothing past it, under a spend cap a day. */
export interface ChatGrant {
  id: number;
  chat: string;
  title: string;
  folder: string;
  expiresAtMs: number | null;
  untilQuit: boolean;
  spentUsd: number;
  capUsd: number;
  spentTokens: number;
  capTokens: number;
  lastRunMs: number | null;
  /** Expired or at its cap for today: the chat asks for everything again. */
  paused: boolean;
  /** You (or its 30-minute deadline) paused the chat itself: no wakes or timers until you resume it. */
  held: boolean;
  /** Its timers: yours, and the ones it set itself (wake_me). */
  timers: ChatTimer[];
}

/** A timer on a granted chat (runtime-kernel §15.4). */
export interface ChatTimer {
  id: number;
  /** "person": yours; "chat": one the chat set for itself (its note isn't shown as yours). */
  by: "person" | "chat";
  atMs: number;
  /** Minutes between fires; null: once. */
  everyMinutes: number | null;
  /** A daily timer's local time and zone ("09:00 Europe/Berlin"). */
  dailyAt: string | null;
  note: string;
}

export interface ChatGrants {
  grants: ChatGrant[];
  /** Why your grants file isn't used, if it isn't (it then counts as no grants). */
  problem: string | null;
}

/** Runtime-kernel §14.4: another chat (or Marv) that wrote the same files as a chat in the last 30 minutes. */
export interface OverlapWith {
  /** The other desktop chat's id, when it is one. */
  chat: string | null;
  /** Its title, or its mesh name. */
  name: string;
  /** The paths both wrote, relative to this chat's folder when inside it. Paths only, never content. */
  paths: string[];
}

export interface WriteOverlap {
  chat: string;
  title: string;
  with: OverlapWith[];
}

/** "also editing here: api work (2 files)", or null when nobody is. */
export function overlapText(o: WriteOverlap | null | undefined): string | null {
  if (!o || o.with.length === 0) return null;
  const files = (n: number) => `${n} file${n === 1 ? "" : "s"}`;
  return `also editing here: ${o.with.map((w) => `${w.name} (${files(w.paths.length)})`).join(", ")}`;
}

/** What a grant spent today of its cap: dollars, or tokens where the requests had no price (a plan, a local model). */
/** One meter in the Usage view (runtime-kernel §16.9): a folder, chat, core or grant, today and this week. */
export interface UsageRow {
  kind: "folder" | "chat" | "core" | "grant";
  key: string;
  name: string;
  todayUsd: number;
  weekUsd: number;
  todayTokens: number;
  weekTokens: number;
  todayRequests: number;
}

/** A budget you set, with today's spend against it. */
export interface BudgetView {
  scope: "global" | "folder" | "chat";
  target: string;
  name: string;
  capUsd: number;
  spentUsd: number;
  percent: number;
  warning: boolean;
  reached: boolean;
}

export interface UsageView {
  todayUsd: number;
  weekUsd: number;
  todayRequests: number;
  rows: UsageRow[];
  budgets: BudgetView[];
  /** Why your budgets file isn't used (it then counts as reached). */
  problem: string | null;
}

/** One agent and model's usage, from the agent's own history on this computer (Claude Code, Codex, OpenCode). */
export interface AgentUsageLine {
  agent: string;
  model: string;
  requests: number;
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  /** At list prices, or as the agent says; null: no price known for the model. */
  usdMicros: number | null;
  /** Some requests had no known price; the cost counts only the others. */
  partlyPriced: boolean;
}

export interface AgentUsage {
  sinceMs: number;
  lines: AgentUsageLine[];
  /** [UTC day start ms, priced micro-dollars], oldest first. */
  days: [number, number][];
  problems: string[];
}

/** One kept turn of a chat's agent (checkpoints): the files before and after it, and what it changed. */
export interface ChatTurn {
  n: number;
  startedMs: number;
  endedMs: number;
  before: string;
  after: string;
  /** [A|M|D, path in the repository]. */
  files: [string, string][];
  added: number;
  removed: number;
  /** "Restored to before turn 4" for a restore. */
  label?: string | null;
}

export function grantSpend(g: ChatGrant): string {
  if (g.spentUsd === 0 && g.spentTokens > 0) {
    const k = (n: number) => (n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : `${Math.round(n / 1000)}k`);
    return `${k(g.spentTokens)} of ${k(g.capTokens)} tokens today`;
  }
  return `$${g.spentUsd.toFixed(2)} of $${g.capUsd.toFixed(2)} today`;
}

/** A chat running in the background, or on screen: whether a turn runs, and how many approvals wait. */
export interface LiveChat {
  id: string;
  busy: boolean;
  waiting: number;
  /** Its session is unloaded but it's on the mesh: a message on a wire wakes it (runtime-kernel 1). */
  asleep: boolean;
  /** Why a message couldn't wake it (its folder is gone), if one couldn't. */
  cantWake: string | null;
  /** Paused (by you, or its 30-minute deadline): no wakes or timers until you resume it or write to it. */
  paused: boolean;
}

/** Runtime-kernel §14.1: the git worktree a chat granted beside another works in (its own branch). */
export interface ChatWorktree {
  dir: string;
  branch: string;
  base: string;
  top: string;
}

/** Runtime-kernel §20.9: one agent's last minute (model / tools / waiting for you), as its Marv measured it. */
export interface Metrics {
  model_pct: number;
  tools_pct: number;
  you_pct: number;
  tokens_per_s?: number;
  cache_hit_pct?: number;
  usd_micros_per_hour?: number;
  requests_per_min: number;
  retries: number;
  throttles: number;
  rate_limit?: { requests_limit?: number; requests_left?: number; tokens_limit?: number; tokens_left?: number };
}

export interface ChildProcess {
  kind: "process" | "mcp";
  name: string;
  pid: number;
  rss_kb?: number;
  cpu_pct?: number;
  alive?: boolean;
}

/** mk-60m.11: a background process the chat started with the process tool (`chat_processes`): its forwarded URLs,
 * state and command, for the dock's Terminal tab. */
export interface ChatProcess {
  name: string;
  command: string;
  state: "running" | "exited";
  exit_code: number | null;
  urls: string[];
  network: boolean;
  notes: string[];
  started_at: number;
}

export interface Live {
  core?: string;
  context?: { used: number; budget: number };
  metrics?: Metrics;
  spend_usd_micros?: number;
  paused: boolean;
  subagents?: { kind: string; phase: "idle" | "model" | "tools" | "you" }[];
  processes?: ChildProcess[];
}

/** One agent on this machine (runtime-kernel §20.1): a desktop chat or a terminal Marv, from its door. */
export interface AgentRow {
  name: string;
  folder: string;
  kind: "chat" | "terminal";
  chat?: string;
  pid: number;
  state?: "sleeping" | "loading" | "idle" | "busy" | "waiting";
  title?: string;
  last_active_ms?: number;
  live: Live;
}

export interface ModelBar {
  model: string;
  agents: number;
  tokens_per_s: number;
  usd_micros_per_hour: number;
  cache_hit_pct?: number;
  rate_limit_pct?: number;
  throttles: number;
}

export interface ActivityView {
  activity: { rows: AgentRow[]; bar: { loaded: number; cap: number; models: ModelBar[]; spend_usd_micros: number } };
  teams: { rule: number; label: string; names: string[] }[];
}

/** One timeline line (runtime-kernel §20.3): a fixed sentence, never a message's words. */
export interface LogLine {
  at_ms: number;
  kind: string;
  chat?: string;
  agents: string[];
  text: string;
}

/** "While you were away" (§20.3), from counts alone. */
export interface Away {
  from_ms: number;
  to_ms: number;
  chats: number;
  calls: number;
  wakes: number;
  needs_you: number;
  paused: number;
  spent_usd_micros?: number;
}

/** A notification shown this run (§20.5), kept in memory. */
export interface Note {
  at_ms: number;
  kind: "needs_you" | "finished" | "paused";
  chat: string;
  name: string;
}

/** What a chat may do (runtime-kernel §20.4), each line with what revokes it. */
export interface ChatPermissions {
  chat: string;
  name: string;
  may: { text: string; revoke: { kind: "rule" | "grant" | "timer"; id: number } }[];
}

/** About this Marv (§20.7). */
export interface AboutMarv {
  version: string;
  limits: { what: string; value: string }[];
  stores: { what: string; path: string }[];
  tool_abi?: string;
}

/** Kernel #41: the watchdog's hang reports in the app's data folder (local only, never uploaded). */
export interface HangReports {
  count: number;
  dir: string;
}

/** Kernel #53: a file in a chat's folder that a desktop chat wrote in the last 30 minutes (paths only). */
export interface WriteMark {
  /** Relative to the chat's folder. */
  path: string;
  /** The writer's title. */
  by: string;
  chat: string | null;
  /** This chat wrote it. */
  mine: boolean;
  atMs: number;
}

/** Kernel #53: one file of a chat's changes (its git diff), with its lines. */
export interface FileChange {
  path: string;
  status: "modified" | "added" | "deleted" | "renamed" | "binary" | "untracked";
  added: number;
  removed: number;
  lines: { kind: "add" | "del" | "ctx" | "gap"; text: string }[];
  more: number;
}

export interface Changes {
  /** "HEAD" (what isn't committed yet), or a worktree chat's base hash (its branch's work). */
  against: string;
  files: FileChange[];
  more_files: number;
  problem: string | null;
}

/** Marv's own settings the desktop shows (~/.marv/config.json, shared with `marv`). */
export interface MarvConfig {
  /** The model `marv` in a terminal uses (`model`): read-only here, the desktop keeps its own. */
  model: string | null;
  smallModel: string | null;
  /** `mesh.wake`: true any wired message wakes a chat, false none does, null only a rule's (the default). */
  meshWake: boolean | null;
  meshRadio: boolean;
  meshCards: boolean;
  problem: string | null;
}

export interface MarvConfigPatch {
  smallModel?: string;
  meshWake?: "on" | "off" | "rules";
  meshRadio?: boolean;
  meshCards?: boolean;
}

/** A harness Marv OS can run chats with (the harness plan): `path` null when it isn't installed. */
/** A coding agent for a chat's terminal (`terminal_agents`). */
export interface TerminalAgent {
  id: string;
  name: string;
  installed: boolean;
  install: string;
  /** Its winget package, on Windows only: the new chat's picker offers Install for it. */
  winget: string | null;
  /** One Marv OS knows, one the person added (Settings > Agents), or their shell (a plain terminal). */
  kind: "known" | "own" | "shell";
}

/** An agent the person added: its name and the command that starts it (`resume`: the arguments that continue it). */
export interface OwnAgent {
  id: string;
  name: string;
  command: string;
  resume: string;
}

export interface HarnessInfo {
  /** Its record name: "claude-code", "opencode", "gemini", "antigravity". */
  id: string;
  name: string;
  path: string | null;
  version: string | null;
  login: "api_key" | "signed_in" | "signed_out" | "unknown";
  loaded: number;
  asleep: number;
  /** Phase 4: its chats run inside Marv's outer sandbox (your setting), and whether that sandbox can run here. */
  sandbox: boolean;
  sandboxAvailable: boolean;
}

/** A worktree chat's branch beside its folder's (`worktrees::WorktreeStatus`). */
export interface WorktreeStatus {
  branch: string;
  into: string | null;
  commits: number;
  uncommitted: number;
  files: number;
  folderChanges: number;
  gone: boolean;
}

export interface ChatRecord {
  id: string;
  /** The full path of the folder the chat runs in. */
  folder: string;
  title: string;
  sessionId?: string | null;
  created: number;
  /** "Private: other chats can't read it" (runtime-kernel §9.2): no chat_read is served for it, whatever the rules. */
  private?: boolean;
  /** Runtime-kernel §14.1: the git worktree it works in, when it has one (⎇ in the sidebar). */
  worktree?: ChatWorktree | null;
  /** The role it was started as (marv_session::roles), fixed for its life; none: no role. */
  role?: string | null;
  /** The team the New team dialog started it in (its roster coordinator first); "End team" clears it. */
  team?: { name: string; roster: string[] } | null;
  /** The write paths it was given when its team started (enforced for its life); none: its role's. */
  writes?: string[] | null;
  /** How it last ran (a team member: from the dialog, so its own model). */
  settings?: { model: string; maxSteps: number; yolo: boolean } | null;
  /** What runs it: "claude-code" for the person's own Claude Code; none: Marv. Fixed for its life. */
  harness?: string | null;
  /** How much its agent may do without asking ("supervised", "auto-edits", "auto", "full"); none: its own settings. */
  permissions?: string | null;
  /** Made by "Report a problem": it works in the report's own folder and is listed with the Home chats. */
  triage?: boolean;
}

/** A team a folder may start (the New team dialog's cards): built-in, yours, or the folder's .marv/team.md. */
export interface TeamEntry {
  name: string;
  title: string;
  description: string;
  source: "built-in" | "personal" | "project";
  members: { role: string; model: string | null }[];
  worktrees: boolean;
  readChats: boolean;
  /** It has a shared brief (its file's body), given to every member after its role. */
  brief: boolean;
  /** The brief's text (shown before Save copies it into the project). */
  briefText: string;
}

/** A team started in a folder: its live rules and its chats (start order). */
export interface TeamHere {
  name: string;
  rules: number[];
  chats: string[];
}

export interface TeamList {
  teams: TeamEntry[];
  problems: string[];
  here: TeamHere[];
  /** The folder has a .marv/team.md (Save asks before replacing it). */
  hasFile: boolean;
  /** mesh.wake now: Start team turns "off" into "rules" for every chat. */
  meshWake: "on" | "off" | "rules";
  /** The roles that have a file in the folder's .marv/roles (Save leaves those alone). */
  roleFiles: string[];
}

/** What the dialog sends (lib/teams.ts builds it). */
export interface TeamSpec {
  name: string;
  description: string | null;
  members: { role: string; title: string; model: string | null; writes: string[] | null }[];
  worktrees: boolean;
  readChats: boolean;
  keepWorking: boolean;
  defaultModel: string;
  maxSteps: number;
  yolo: boolean;
}

/** One step of a team's start. */
export interface TeamStep {
  what: string;
  ok: boolean;
  detail: string;
}

export interface TeamStarted {
  steps: TeamStep[];
  chats: ChatRecord[];
  rules: number[];
  ok: boolean;
}

export interface SavedTeam {
  written: string[];
  kept: string[];
  /** .marv/team.md is there: nothing written (confirm, then save with `replace`). */
  exists: boolean;
  /** The written files joined the folder's trust. */
  trusted: boolean;
}

/** A role a new chat may start as (the desktop's `list_roles`). */
export interface RoleEntry {
  name: string;
  description: string;
  source: "built-in" | "personal" | "project";
  /** The tools it's limited to; null: every tool (but those in `except`). */
  tools: string[] | null;
  except: string[];
  /** The paths it may write under; null: anywhere; empty: nothing. */
  writes: string[] | null;
  takesTasksFrom: string[];
}

/** After adding a server: when chats get it (the desktop's `marv_core::APPLIES_AT_NEXT_START`). */
export const NEW_SERVER_NOTE = "Added. Applies to new chats: an open chat keeps the tools it started with until it next starts.";

export interface ChatSettings {
  model: string;
  maxSteps: number;
  /** Sandboxed commands and edits in the folder run without asking. */
  yolo: boolean;
  /** MCP servers switched off for chats (by name). */
  mcpOff: string[];
}

/** An MCP server as the config files have it. */
/** One of your apps (~/.marv/mcp.json) as the Apps place shows it. */
export interface AppStatus {
  name: string;
  /** "oauth" signs in; "key" sends its own Authorization header; "local" runs on this computer. */
  kind: "oauth" | "key" | "local";
  signedIn: boolean;
  /** Where it signed in ("accounts.google.com"). */
  host: string | null;
  /** Holds an OAuth client of its own (another app of its provider can reuse it). */
  hasClient: boolean;
}

export interface OAuthSetup {
  clientId?: string;
  clientSecret?: string;
  scope?: string;
  callbackPort?: number;
}

export interface ServerEntry {
  name: string;
  scope: "global" | "folder";
  target: string;
}

/** A looked-up server to add, shown before it's written; or several to choose from. */
export type Lookup =
  | { name: string; title: string; command?: string | null; text: string; warning?: string | null }
  | { choices: string[] };

export type ServerEvent = Record<string, unknown> & { type: string };

export interface Connection {
  send(message: object): void;
  close(): void;
}

export const desktop = {
  info: () => invoke<AppInfo>("app_info"),
  setRoot: (path: string) => invoke<AppInfo>("set_root", { path }),
  rootUp: () => invoke<AppInfo>("root_up"),
  /** Go into a folder: it becomes the tree's top folder. */
  enterFolder: (rel: string) => invoke<AppInfo>("enter_folder", { rel }),
  /** `marv-desktop <dir>`: where to open, once. */
  takeLaunch: () => invoke<{ info: AppInfo; folder: string } | null>("take_launch"),
  setApiKey: (key: string) => invoke<AppInfo>("set_api_key", { key }),
  setZaiKey: (key: string) => invoke<AppInfo>("set_zai_key", { key }),
  setKimiKey: (key: string) => invoke<AppInfo>("set_kimi_key", { key }),
  /** Opens OpenAI's sign-in page in the browser and waits for it (a 127.0.0.1 listener takes the one callback). */
  chatgptSignIn: (another?: boolean) => invoke<AppInfo>("chatgpt_sign_in", { another }),
  /** The Marv plan (app/src-tauri/src/plan.rs): every call to its server goes from the Rust side. */
  planLoginStart: (email: string) => invoke<void>("plan_login_start", { email }),
  /** `marketing`: the person ticked "Email me about Marv updates" (consent). */
  planLoginVerify: (email: string, code: string, marketing = false) => invoke<PlanStatus>("plan_login_verify", { email, code, marketing }),
  /** Sign in with Google or GitHub: opens the browser and waits (up to 5 minutes) for it to come back. */
  planLoginWith: (provider: "google" | "github", marketing = false) => invoke<PlanStatus>("plan_login_with", { provider, marketing }),
  /** Product emails on or off for the signed-in account. */
  planSetMarketing: (on: boolean) => invoke<PlanStatus>("plan_set_marketing", { on }),
  planLoginCancel: () => invoke<void>("plan_login_cancel"),
  /** The first run's "Sign in to Marv" step, skipped. */
  accountStepDone: () => invoke<AppInfo>("account_step_done"),
  /** `online: false`: from plan.json and the licence only, no call. */
  planStatus: (online = true) => invoke<PlanStatus>("plan_status", { online }),
  /** `notice`: a key stayed that this computer couldn't confirm as the plan's. */
  planLogout: () => invoke<{ info: AppInfo; notice: string | null }>("plan_logout"),
  planManage: () => invoke<void>("plan_manage"),
  planUseKey: () => invoke<AppInfo>("plan_use_key"),
  /** Stops a sign-in still waiting for the browser. */
  chatgptCancelSignIn: () => invoke<void>("chatgpt_cancel_sign_in"),
  chatgptSignOut: () => invoke<AppInfo>("chatgpt_sign_out"),
  planModels: () => invoke<PlanModel[]>("plan_models"),
  listDir: (rel: string) => invoke<Entry[]>("list_dir", { rel }),
  createFolder: (parent: string, name: string) => invoke<string>("create_folder", { parent, name }),
  rename: (rel: string, name: string) => invoke<string>("rename_entry", { rel, name }),
  trash: (rel: string) => invoke<void>("trash_entry", { rel }),
  chats: (folder?: string) => invoke<ChatRecord[]>("list_chats", { folder }),
  createChat: (folder: string, title: string, role?: string | null, harness?: string | null, worktree?: boolean) =>
    invoke<ChatRecord>("create_chat", { folder, title, role: role || null, harness: harness || null, worktree: worktree ?? null }),
  /** Whether the person's own Claude Code (`claude`) is installed, for the new chat's agent picker. */
  claudeCodeAvailable: () => invoke<boolean>("claude_code_available"),
  /** Whether `folder` is in a git repository with a commit: a new chat there may work in its own worktree. */
  folderInGit: (folder: string) => invoke<boolean>("folder_in_git", { folder }),
  /** The harnesses found here (Settings → Harnesses): version, login, chats loaded and asleep. */
  harnesses: () => invoke<HarnessInfo[]>("harnesses"),
  /** Every coding agent a chat's terminal can run (Marv, Claude Code, Codex, OpenCode…), installed or not. */
  terminalAgents: () => invoke<TerminalAgent[]>("terminal_agents"),
  ownAgents: () => invoke<OwnAgent[]>("own_agents"),
  addOwnAgent: (name: string, command: string, resume: string) => invoke<OwnAgent[]>("add_own_agent", { name, command, resume }),
  removeOwnAgent: (id: string) => invoke<OwnAgent[]>("remove_own_agent", { id }),
  /** Windows without `git` on the PATH: checkpoints, turns, worktrees and the Git tab need it. */
  gitMissing: () => invoke<boolean>("git_missing"),
  /** Installs an agent's winget package (or `Git.Git`) and says what winget said; only those Marv OS offers. */
  installPackage: (id: string) => invoke<string>("install_package", { id }),
  /** Turns the outer sandbox for harness chats on or off: from each chat's next start. */
  setHarnessSandbox: (on: boolean) => invoke<HarnessInfo[]>("set_harness_sandbox", { on }),
  /** The roles a new chat in `folder` may start as, and the role files that couldn't be loaded. */
  listRoles: (folder: string) => invoke<[RoleEntry[], string[]]>("list_roles", { folder }),
  /** The teams the New team dialog offers in `folder`, and the teams started there. */
  listTeams: (folder: string) => invoke<TeamList>("list_teams", { folder }),
  /** Your click: the folder rule(s), wake, each chat with its role and model (coordinator first); each step's result. */
  startTeam: (folder: string, spec: TeamSpec) => invoke<TeamStarted>("start_team", { folder, spec }),
  /** Your click: the team's rules are revoked; its chats stay as normal chats with their roles. */
  endTeam: (folder: string, name: string) => invoke<MeshRules>("end_team", { folder, name }),
  /** Your click, after a confirm: .marv/team.md and the missing .marv/roles files (`replace`: over an existing team.md). */
  saveTeamFile: (folder: string, spec: TeamSpec, replace = false) => invoke<SavedTeam>("save_team_file", { folder, spec, replace }),
  /** Each member's latest report-back to the team's lead (from the lead's saved conversation), for the Team view. */
  teamReports: (lead: string, ids: string[]) => invoke<{ chat: string; text: string }[]>("team_reports", { lead, ids }),
  renameChat: (id: string, title: string) => invoke<void>("rename_chat", { id, title }),
  /** A note when the chat's worktree and branch were kept (work only they have). */
  deleteChat: (id: string) => invoke<string | null>("delete_chat", { id }),
  /** Your click: other chats can't (or can again) read this one, whatever the rules. */
  setChatPrivate: (id: string, priv: boolean) => invoke<void>("set_chat_private", { id, private: priv }),
  /** Your pick of a chat's permission mode, from its next start; the mode it will run with (a home folder holds it
   * at supervised). */
  setChatPermissions: (id: string, mode: string) => invoke<string>("set_chat_permissions", { id, mode }),
  /** Settings' permission mode for new chats ("own": each agent's own settings). */
  setDefaultPermissions: (mode: string) => invoke<void>("set_default_permissions", { mode }),
  defaultPermissions: () => invoke<string>("default_permissions"),
  /** Ends a chat running in the background: it leaves the mesh. Opening it again continues it. */
  stopChat: (id: string) => invoke<void>("stop_chat", { id }),
  /** The running chats (#155), shown or not. */
  liveChats: () => invoke<LiveChat[]>("live_chats"),
  /** Runtime-kernel §14.4: chats editing the same files as another live chat (last 30 minutes). Shown, never scheduled. */
  writeOverlaps: () => invoke<WriteOverlap[]>("write_overlaps"),
  /** The chat's own worktree, if it works in one. */
  chatWorktree: (id: string) => invoke<ChatWorktree | null>("chat_worktree", { id }),
  /** What a worktree chat's branch has beside its folder's branch. */
  worktreeStatus: (id: string) => invoke<WorktreeStatus | null>("worktree_status", { id }),
  /** Merge a worktree chat's branch into its folder's, then remove its worktree, branch and chat. What it did. */
  worktreeMerge: (id: string) => invoke<string>("worktree_merge", { id }),
  /** Remove a worktree chat's worktree, branch and chat, its work with them. */
  worktreeDiscard: (id: string) => invoke<string>("worktree_discard", { id }),
  /** Your click: the chat works in its folder again, in a new conversation. Its branch is kept (the answer says how
   * to merge or remove it). */
  moveChatToFolder: (id: string) => invoke<string>("move_chat_to_folder", { id }),
  /** Kernel #53, the dock: a folder in the chat's own root (its worktree when it has one). */
  chatFiles: (id: string, rel: string) => invoke<Entry[]>("chat_files", { id, rel }),
  /** The files in the chat's root a desktop chat wrote in the last 30 minutes, with who. */
  chatWrites: (id: string) => invoke<WriteMark[]>("chat_writes", { id }),
  /** The chat's git diff (read-only). */
  chatChanges: (id: string) => invoke<Changes>("chat_changes", { id }),
  /** mk-60m.11: the background processes the chat started (empty while it isn't loaded). */
  chatProcesses: (id: string) => invoke<ChatProcess[]>("chat_processes", { id }),
  /** A process's last lines, as the process tool's `logs` reads them. */
  chatProcessLogs: (id: string, name: string) => invoke<string>("chat_process_logs", { id, name }),
  /** Your click (confirmed first): the process and what it started end. `startedAt` is the start you saw: one started
   * again since is refused (an error), never stopped in its place. False when it was already gone. */
  stopChatProcess: (id: string, name: string, startedAt: number) => invoke<boolean>("stop_chat_process", { id, name, startedAt }),
  marvConfig: () => invoke<MarvConfig>("marv_config"),
  setMarvConfig: (patch: MarvConfigPatch) => invoke<MarvConfig>("set_marv_config", { patch }),
  /** Runtime-kernel §20.1: every agent on this machine, for the Activity tab. */
  activity: () => invoke<ActivityView>("activity"),
  timeline: (sinceMs: number) => invoke<LogLine[]>("timeline", { sinceMs }),
  awayLeave: () => invoke<void>("away_leave"),
  awayBack: () => invoke<Away | null>("away_back"),
  notifications: () => invoke<Note[]>("notifications"),
  permissions: () => invoke<ChatPermissions[]>("permissions", { utcOffsetMinutes: -new Date().getTimezoneOffset() }),
  about: () => invoke<AboutMarv>("about_marv"),
  /** Kernel #41: what this launch did at start, once ("an earlier Marv window stopped responding; it was closed"). */
  watchdogNotice: () => invoke<string | null>("watchdog_notice"),
  /** Kernel #41: how many hang reports there are, and where. */
  hangReports: () => invoke<HangReports>("hang_reports"),
  /** Kernel #41: the reports' folder in the file manager. */
  openHangReports: () => invoke<void>("open_hang_reports"),
  /** The app's log folder in the file manager. */
  logsOpen: () => invoke<void>("logs_open"),
  /** The log's recent lines, for a problem report. */
  /** Your switch of a chat to another agent, in place: the one taking over gets the conversation so far; the text to
   * type into its terminal. */
  switchAgent: (id: string, agent: string) => invoke<string>("switch_agent", { id, agent }),
  /** A chat's kept turns, newest last; an error outside a git repository. */
  chatTurns: (id: string) => invoke<ChatTurn[]>("chat_turns", { id }),
  /** Turn `n`'s own diff. */
  chatTurnDiff: (id: string, n: number) => invoke<string>("chat_turn_diff", { id, n }),
  /** Your click: the files back as they were before turn `n` (kept as a turn, so it can be undone); what was done. */
  chatTurnRestore: (id: string, n: number) => invoke<string>("chat_turn_restore", { id, n }),
  /** "Report a problem": a folder with the playbook, the log and what's known about this computer, and a chat there run
   * by `harness` (none: Marv); the chat and the first message to type into its terminal. */
  startTriage: (harness: string | null) => invoke<[ChatRecord, string]>("start_triage", { harness }),
  logsRecent: () => invoke<string>("logs_recent"),
  /** The desktop app's own version (Marv OS, e.g. 1.0.0-beta.1): tauri.conf's, which the release build stamps. */
  appVersion: () => getVersion(),
  /** Your mesh rules (~/.marv/mesh/policies.json). */
  meshRules: () => invoke<MeshRules>("mesh_rules"),
  /** Your click: chats under `rel` may message each other, for `lasts` ("revoked" | "hour" | "quit"); with
   * `keepWorking`, the chats there now also keep working while you're away (one grant each); with `read`, they may
   * also read each other's chats (a second rule, in the read mode); with `worktrees` (on by default), chats granted
   * there that share a folder each start in their own git worktree (runtime-kernel §14.1). */
  allowFolder: (rel: string, lasts: string, keepWorking: boolean, read = false, worktrees = true) =>
    invoke<MeshRules>("allow_folder", { rel, lasts, keepWorking, read, worktrees }),
  revokeRule: (id: number) => invoke<MeshRules>("revoke_rule", { id }),
  /** Runtime-kernel §19: a rule's team board (~/.marv/mesh/boards/<rule>.json); your post (as you) and your Clear. */
  /** Several rules' boards as one (a team in worktrees has one per pair of its folders); your post goes to each. */
  board: (rules: number[]) => invoke<Board>("board", { rules }),
  boardPost: (rules: number[], kind: string, text: string) => invoke<Board>("board_post", { rules, kind, text }),
  /** One entry (its `refs`), or (no `entry`) every one of these boards. */
  boardClear: (rules: number[], entry?: [number, number][]) => invoke<Board>("board_clear", { rules, entry }),
  /** Your grants (~/.marv/mesh/grants.json): chats that keep working while you're away. */
  chatGrants: () => invoke<ChatGrants>("chat_grants"),
  revokeGrant: (id: number) => invoke<ChatGrants>("revoke_grant", { id }),
  /** Your click: a grant's spend cap a day, in dollars. */
  setGrantCap: (id: number, usd: number) => invoke<ChatGrants>("set_grant_cap", { id, usd }),
  usageView: () => invoke<UsageView>("usage_view"),
  /** The agents' usage over the last `days` days, from their own history (in Marv OS or not): an estimate at list prices. */
  agentUsage: (days: number) => invoke<AgentUsage>("agent_usage", { days }),
  /** Your click: a budget in dollars a day for everything, a folder or a chat; `usd` null removes it. */
  setBudget: (scope: "global" | "folder" | "chat", target: string | null, usd: number | null) =>
    invoke<UsageView>("set_budget", { scope, target, usd }),
  /** Your signal to a chat (stop, pause, resume, close): only from here, never from another chat. */
  chatSignal: (id: string, kind: "stop" | "pause" | "resume" | "close") => invoke<ChatGrants>("chat_signal", { id, kind }),
  /** Pauses and stops every chat a rule covers; how many were running. */
  stopTeam: (id: number) => invoke<number>("stop_team", { id }),
  /** Resumes every paused chat a rule covers; how many were paused. */
  resumeTeam: (id: number) => invoke<number>("resume_team", { id }),
  /** `daily`: at a local time in your zone, kept across DST. */
  addTimer: (chat: string, atMs: number, everyMinutes: number | null, note: string, daily?: { minute: number; tz: string }) =>
    invoke<ChatGrants>("add_timer", { chat, atMs, everyMinutes, note, dailyMinute: daily?.minute ?? null, tz: daily?.tz ?? null }),
  removeTimer: (id: number) => invoke<ChatGrants>("remove_timer", { id }),
  mcpServers: (folder: string) => invoke<ServerEntry[]>("mcp_servers", { folder }),
  mcpLookup: (source: string, name?: string, vars?: string[], headers?: Record<string, string>) =>
    invoke<Lookup>("mcp_lookup", { source, name, vars, headers }),
  /** Writes the looked-up server, its `${VAR}`s filled from `values`. */
  mcpInstall: (name: string, values?: Record<string, string>) => invoke<void>("mcp_install", { name, values }),
  createSpace: (name: string, path: string) => invoke<AppInfo>("create_space", { name, path }),
  switchSpace: (id: string) => invoke<AppInfo>("switch_space", { id }),
  renameSpace: (id: string, name: string) => invoke<AppInfo>("rename_space", { id, name }),
  removeSpace: (id: string) => invoke<AppInfo>("remove_space", { id }),
  setAppsOff: (off: string[]) => invoke<AppInfo>("set_apps_off", { off }),
  /** A chat's devices switched off (MCP servers as `mcp:<name>`, and the doors): read when its session next starts. */
  /** The chat's devices (doors, then its own folder's servers and yours) and which are off. */
  chatDevices: (id: string) => invoke<{ names: string[]; off: string[] }>("chat_devices", { id }),
  setChatDevices: (id: string, off: string[]) => invoke<string[]>("set_chat_devices", { id, off }),
  setProvider: (provider: "openrouter" | "ollama" | "zai" | "kimi" | "chatgpt") => invoke<AppInfo>("set_provider", { provider }),
  localModels: () => invoke<LocalModel[]>("local_models"),
  /** The dock's Git tab: the folder's lazygit (the chat's folder, else the tree folder `folder`), its output on `onData`. */
  gitTerminalOpen: (chat: string | null, folder: string, cols: number, rows: number, onData: (m: { data?: string; exit?: boolean }) => void) => {
    const channel = new Channel<{ data?: string; exit?: boolean }>();
    channel.onmessage = onData;
    return invoke<number>("git_terminal_open", { chat, folder, cols, rows, onData: channel });
  },
  gitTerminalWrite: (id: number, data: string) => invoke<void>("git_terminal_write", { id, data }),
  chatDeliverMail: (id: string) => invoke<void>("chat_deliver_mail", { id }),
  /** Running Marv chat `id` started on an older provider than Marv's config names now: what to tell the person. */
  chatProviderChanged: (id: string) => invoke<string | null>("chat_provider_changed", { id }),
  clipboardWrite: (text: string) => invoke<void>("clipboard_write", { text }),
  clipboardRead: () => invoke<string>("clipboard_read"),
  /** Whether the clipboard holds a picture (a screenshot), for a terminal's paste: it has no text to paste. */
  clipboardHasImage: () => invoke<boolean>("clipboard_has_image"),
  gitTerminalResize: (id: number, cols: number, rows: number) => invoke<void>("git_terminal_resize", { id, cols, rows }),
  gitTerminalDetach: (id: number) => invoke<void>("git_terminal_detach", { id }),
  gitTerminalClose: (id: number) => invoke<void>("git_terminal_close", { id }),
  gitTerminalPopOut: (chat: string | null, folder: string) => invoke<void>("git_terminal_pop_out", { chat, folder }),
  /** Whether the Git tab's folder is a git repository (lazygit runs only in one). */
  gitRepoHere: (chat: string | null, folder: string) => invoke<boolean>("git_repo_here", { chat, folder }),
  /** `git init` in the Git tab's folder. */
  gitInitHere: (chat: string | null, folder: string) => invoke<void>("git_init_here", { chat, folder }),
  /** A Claude Code chat's terminal view: its conversation in Claude Code's own interface (the chat sleeps meanwhile). */
  chatTerminalOpen: (id: string, cols: number, rows: number, onData: (m: { data?: string; exit?: boolean }) => void) => {
    const channel = new Channel<{ data?: string; exit?: boolean }>();
    channel.onmessage = onData;
    return invoke<number>("chat_terminal_open", { id, cols, rows, onData: channel });
  },
  chatTerminalClose: (id: string) => invoke<void>("chat_terminal_close", { id }),
  /** The person's message for a chat's agent from another view: typed into its terminal (started if it isn't running). */
  chatTerminalType: (id: string, text: string) => invoke<void>("chat_terminal_type", { id, text }),
  chatTerminalRunning: (id: string) => invoke<boolean>("chat_terminal_running", { id }),
  pullModel: (name: string, onProgress: (line: string) => void) => {
    const channel = new Channel<string>();
    channel.onmessage = onProgress;
    return invoke<void>("pull_model", { name, onProgress: channel });
  },
  mcpRemove: (name: string) => invoke<void>("mcp_remove", { name }),
  appsStatus: () => invoke<AppStatus[]>("apps_status"),
  /** Adds an app that signs in to ~/.marv/mcp.json; `reuse` names one of yours whose OAuth client it takes. */
  appConnect: (name: string, url: string, oauth?: OAuthSetup, reuse?: string) => invoke<void>("app_connect", { name, url, oauth, reuse }),
  /** Opens the provider's sign-in in the browser; resolves with the app's tool count once the person is back. */
  appSignIn: (name: string) => invoke<number>("app_sign_in", { name }),
  appCancelSignIn: () => invoke<void>("app_cancel_sign_in"),
  appSignOut: (name: string) => invoke<void>("app_sign_out", { name }),
  /** Voxtype dictation on or off: it types into the focused field. */
  dictate: () => invoke<void>("dictate"),
  /** An https link, in the person's browser. */
  openUrl: (url: string) => invoke<void>("open_url", { url }),
};

/** Opens a chat's session; `onEvent` gets its events until `close()`. */
export async function connect(
  id: string,
  settings: ChatSettings,
  resume: { generation: number; after: number; epoch: string | null },
  onEvent: (e: ServerEvent) => void,
): Promise<Connection> {
  if (isDesktop) {
    const channel = new Channel<ServerEvent>();
    channel.onmessage = onEvent;
    // A chat still running in the background is shown again from event `after` on; it isn't started twice.
    await invoke("open_chat", { id, ...settings, ...resume, onEvent: channel });
    return {
      send: (message) => void invoke("chat_message", { id, message }).catch((e) => onEvent({ type: "error", message: String(e) })),
      // The chat keeps running (on the mesh, its turn going on); stop_chat ends it.
      close: () => void invoke("close_chat", { id }),
    };
  }
  // A browser: marv-web, one WebSocket per chat.
  const url = import.meta.env.VITE_MARV_WS ?? `ws://${location.hostname}:8787/ws`;
  const ws = new WebSocket(url);
  const queue: string[] = [];
  ws.onopen = () => {
    ws.send(JSON.stringify({ type: "hello", chatId: id, ...settings }));
    queue.splice(0).forEach((m) => ws.send(m));
  };
  ws.onmessage = (m) => onEvent(JSON.parse(m.data));
  ws.onclose = () => onEvent({ type: "closed" });
  return {
    send: (message) => {
      const text = JSON.stringify(message);
      if (ws.readyState === WebSocket.OPEN) ws.send(text);
      else queue.push(text);
    },
    close: () => ws.close(),
  };
}

export interface Model {
  id: string;
  name: string;
  context: number;
  /** Dollars per million tokens. */
  input: number;
  output: number;
  /** Runs on this machine (Ollama). */
  local?: boolean;
  /** Paid by a plan set up here ("Z.ai Coding Plan", "ChatGPT"): no per-token price. */
  plan?: string;
}

/** The models of the plans set up here (Z.ai Coding Plan, ChatGPT). */
export async function planModels(): Promise<Model[]> {
  if (!isDesktop) return [];
  try {
    return (await desktop.planModels()).map((m) => ({
      id: m.id,
      name: m.name ?? m.id,
      context: m.context ?? 0,
      input: 0,
      output: 0,
      plan: m.plan,
    }));
  } catch {
    return [];
  }
}

/** The models pulled into the local Ollama, as `ollama/<name>`; none when it isn't running. */
/** The models in the local Ollama (asked by the app, not the page: no browser rules in the way). */
export async function localModels(): Promise<Model[]> {
  if (!isDesktop) return [];
  try {
    return (await desktop.localModels()).map((m) => ({
      id: m.id,
      name: m.tools ? m.id.slice(7) : `${m.id.slice(7)} · no tool calls`,
      context: 0,
      input: 0,
      output: 0,
      local: true,
    }));
  } catch {
    return [];
  }
}

const MODELS_KEY = "marv-models-2";

/** Local Ollama models first, then OpenRouter's that can call tools, cheapest first (cached for an hour). */
export async function loadModels(): Promise<Model[]> {
  const [local, plans, remote] = await Promise.all([localModels(), planModels(), remoteModels().catch(() => [] as Model[])]);
  return [...local, ...plans, ...remote];
}

export async function remoteModels(): Promise<Model[]> {
  let cached: { at: number; models: Model[] } | null = null;
  try {
    cached = JSON.parse(localStorage.getItem(MODELS_KEY) ?? "null");
    if (cached && Date.now() - cached.at < 3_600_000) return cached.models;
  } catch {}
  // Offline or an error page: the stale list is better than none (and is not cached again as if it were fresh).
  const stale = cached?.models ?? [];
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let data: any;
  try {
    const response = await fetch("https://openrouter.ai/api/v1/models");
    if (!response.ok) return stale;
    data = (await response.json()).data;
  } catch {
    return stale;
  }
  const models: Model[] = (data ?? [])
    .filter((m: { supported_parameters?: string[] }) => m.supported_parameters?.includes("tools"))
    .map((m: { id: string; name: string; context_length: number; pricing: { prompt: string; completion: string } }) => ({
      id: m.id,
      name: m.name,
      context: m.context_length,
      input: Number(m.pricing.prompt) * 1e6,
      output: Number(m.pricing.completion) * 1e6,
    }))
    // Routers price per request (-1): no fixed price to show.
    .filter((m: Model) => m.input >= 0 && m.output >= 0)
    // Batch variants only take the batch API, not chat.
    .filter((m: Model) => !m.id.endsWith(":batch"))
    .sort((a: Model, b: Model) => a.input - b.input);
  if (models.length === 0) return stale;
  try {
    localStorage.setItem(MODELS_KEY, JSON.stringify({ at: Date.now(), models }));
  } catch {}
  return models;
}
