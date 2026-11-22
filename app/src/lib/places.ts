// Kernel #53: the desktop's places (Home, Folders, Teams, Activity, Apps, and Settings behind the gear) and the small
// pure pieces they share. What these read comes from the desktop's commands; nothing here decides anything a command
// didn't say. No imports (the shapes below are backend.ts's, as far as these read them), so its tests run under plain
// Node.

/** backend.ts's `LiveChat`. */
export interface LiveChat {
  id: string;
  busy: boolean;
  waiting: number;
  asleep: boolean;
  cantWake: string | null;
  paused: boolean;
}

/** backend.ts's `ChatRecord`, as far as these read it. */
export interface ChatRecord {
  id: string;
  folder: string;
  title: string;
  created: number;
}

/** backend.ts's `ChatGrant`, as far as these read it. */
export interface ChatGrant {
  id: number;
  chat: string;
  title: string;
  capUsd: number;
  paused: boolean;
  held: boolean;
}

/** backend.ts's `BudgetView`, as far as these read it. */
export interface BudgetView {
  scope: string;
  target: string;
  name: string;
  capUsd: number;
  reached: boolean;
}

/** backend.ts's `WriteMark`. */
export interface WriteMark {
  path: string;
  by: string;
  chat: string | null;
  mine: boolean;
  atMs: number;
}

export type Place = "home" | "folders" | "teams" | "activity" | "apps" | "settings";

export const PLACES: { id: Exclude<Place, "settings">; label: string; title: string }[] = [
  { id: "home", label: "Home", title: "What needs you, and what happened while you were away" },
  { id: "folders", label: "Folders", title: "Your folders and their chats" },
  { id: "activity", label: "Activity", title: "Every agent on this computer and what it's doing" },
];

export function isPlace(value: unknown): value is Place {
  // Teams and Apps aren't in the bar (the folder tree and Settings open them) but are places still.
  return typeof value === "string" && (["settings", "teams", "apps"].includes(value) || PLACES.some((p) => p.id === value));
}

/** The folder a Home chat is in, as the page names it: the home folder itself, outside every space. */
export const HOME_FOLDER = "~";

/** A Home chat: one that runs in the home folder (not a space's folder). */
/** A chat listed with the Home chats: one in the home folder, or a problem report's (its own folder, outside every
 * space). */
export const isHomeChat = (chat: { folder: string; triage?: boolean }, home: string) => samePath(chat.folder, home) || !!chat.triage;

/** A Windows path (`C:\\…`, `\\\\server\\…`): its separators are `\\` or `/`, and its case doesn't matter. */
const windowsPath = (path: string) => /^[A-Za-z]:[\\/]/.test(path) || path.startsWith("\\\\");

/** `path` as the page compares it: `/` between its parts, no `/` at the end; a Windows path in lower case. */
const pathKey = (path: string) => {
  const slashed = path.replaceAll("\\", "/").replace(/(.)\/+$/, "$1");
  return windowsPath(path) ? slashed.toLowerCase() : slashed;
};

/** Whether two paths are the same folder (on Windows, `C:\\Users\\me` and `c:/users/me` are). */
export const samePath = (a: string, b: string) => pathKey(a) === pathKey(b);

/** `path` relative to `root`, with `/` between its parts (`""`: `root` itself), or null when it isn't inside it. */
export function relUnder(path: string, root: string): string | null {
  const p = pathKey(path);
  const r = pathKey(root);
  if (p === r) return "";
  if (!p.startsWith(r.endsWith("/") ? r : `${r}/`)) return null;
  // Its own spelling, not the lower-cased key: a folder's name shows as it is.
  return path.replaceAll("\\", "/").slice(r.length).replace(/^\/+/, "").replace(/\/+$/, "");
}

/** A chat's state dot: its tone (the colour) and word. */
export type Tone = "working" | "waiting" | "sleeping" | "idle" | "paused" | "guest" | "off";

export function chatTone(live: LiveChat | undefined): { tone: Tone; word: string } {
  if (!live) return { tone: "off", word: "not running" };
  if (live.waiting > 0) return { tone: "waiting", word: "needs you" };
  if (live.paused) return { tone: "paused", word: "paused" };
  if (live.busy) return { tone: "working", word: "working" };
  if (live.asleep) return { tone: "sleeping", word: live.cantWake ? "can't wake" : "sleeping" };
  return { tone: "idle", word: "idle" };
}

export interface ChatGroup<C extends ChatRecord = ChatRecord> {
  /** The folder relative to the space's top folder ("" is the top folder itself). */
  rel: string;
  name: string;
  chats: C[];
}

/**
 * The space's chats grouped by folder (the Folders sidebar): the top folder first, then by name; chats outside the
 * space left out. `selected` (the folder a new chat goes to) is listed even with no chats yet.
 */
export function groupChats<C extends ChatRecord>(chats: C[], root: string, rootName: string, selected: string): ChatGroup<C>[] {
  const groups = new Map<string, C[]>();
  for (const chat of chats) {
    const rel = relUnder(chat.folder, root);
    if (rel === null) continue;
    groups.set(rel, [...(groups.get(rel) ?? []), chat]);
  }
  if (!groups.has(selected) && selected !== HOME_FOLDER) groups.set(selected, []);
  return [...groups.entries()]
    .map(([rel, list]) => ({ rel, name: rel || rootName, chats: [...list].sort((a, b) => b.created - a.created) }))
    .sort((a, b) => (a.rel === "" ? -1 : b.rel === "" ? 1 : a.rel.localeCompare(b.rel)));
}

/**
 * The chats under folder `rel` whose own folder is gone (renamed or removed away): the next folder down their path
 * isn't in `rel`'s listing `entries` (`undefined`: not listed yet, so nothing is known gone). The tree lists only
 * folders that exist, so these had no row and no Delete anywhere (seen 2026-11-17); they show inside `rel` instead.
 */
export function goneFolderChats<C extends ChatRecord>(groups: ChatGroup<C>[], rel: string, entries: { name: string; dir: boolean }[] | undefined): C[] {
  if (entries === undefined) return [];
  const here = new Set(entries.filter((e) => e.dir).map((e) => e.name));
  return groups
    .filter((g) => g.rel !== rel && (rel === "" || g.rel.startsWith(`${rel}/`)))
    .filter((g) => !here.has(g.rel.slice(rel === "" ? 0 : rel.length + 1).split("/")[0]))
    .flatMap((g) => g.chats);
}

/** Something on Home's "Needs you" list: Open goes to the chat, or to a place. */
export interface Need {
  key: string;
  title: string;
  what: string;
  chat?: string;
  place?: Place;
}

const money = (usd: number) => `$${usd.toFixed(2)}`;

/**
 * What needs you, from the commands' answers: chats waiting for an approval (anywhere), grants paused at their cap
 * or held, and budgets reached.
 */
export function needsYou(live: LiveChat[], chats: ChatRecord[], grants: ChatGrant[], budgets: BudgetView[]): Need[] {
  const title = (id: string) => chats.find((c) => c.id === id)?.title || "a chat";
  const out: Need[] = [];
  for (const l of live) {
    if (l.waiting > 0)
      out.push({ key: `ask:${l.id}`, title: title(l.id), what: l.waiting === 1 ? "is waiting for your answer" : `has ${l.waiting} questions waiting`, chat: l.id });
  }
  for (const g of grants) {
    if (g.paused) out.push({ key: `grant:${g.id}`, title: g.title || title(g.chat), what: `paused at its ${money(g.capUsd)} grant cap`, place: "teams" });
    else if (g.held) out.push({ key: `held:${g.id}`, title: g.title || title(g.chat), what: "is paused: mail and timers wait", place: "teams" });
  }
  for (const b of budgets) {
    if (b.reached) out.push({ key: `budget:${b.scope}:${b.target}`, title: `budget · ${b.name}`, what: `reached ${money(b.capUsd)} today: chats ask before going on`, place: "settings" });
  }
  return out;
}

/** Who is editing a file (its own marks) or a folder (any file under it), newest first; null: nobody. */
export function markFor(path: string, dir: boolean, marks: WriteMark[]): { by: string; mine: boolean } | null {
  const hits = marks.filter((m) => (dir ? m.path.startsWith(`${path}/`) : m.path === path));
  if (hits.length === 0) return null;
  const names = [...new Set(hits.map((m) => (m.mine ? "this chat" : m.by)))];
  return { by: names.length > 2 ? `${names.slice(0, 2).join(", ")} +${names.length - 2}` : names.join(", "), mine: hits.some((m) => m.mine) };
}

export function greeting(hour: number): string {
  return hour < 5 ? "Good evening" : hour < 12 ? "Good morning" : hour < 18 ? "Good afternoon" : "Good evening";
}

/** The dock's tabs: the folder's files and its git; a team's chats also its board. (A chat's processes and context
 * went: every chat is its agent's terminal, which shows its own.) */
export const DOCK_TABS = ["files", "git", "board"] as const;
export type DockTab = (typeof DOCK_TABS)[number];

export interface DockPrefs {
  open: boolean;
  tab: DockTab;
}

/** A chat's dock as last left (per chat), else as the last chat's was. */
export function dockFor(saved: Record<string, DockPrefs>, key: string, last: DockPrefs): DockPrefs {
  const mine = saved[key];
  return mine && DOCK_TABS.includes(mine.tab) ? mine : last;
}

/** backend.ts's `ChildProcess` (Activity's row: a real process with its pid). */
export interface ChildProcess {
  kind: "process" | "mcp";
  name: string;
  pid: number;
  rss_kb?: number;
  cpu_pct?: number;
  alive?: boolean;
}

/** backend.ts's `ChatProcess` (mk-60m.11: what the chat started with the process tool). */
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

/** One line of the dock's Terminal tab: a real process (its pid, CPU, memory) and, for one the process tool started,
 * its command, URLs and state, so it can show its logs and be stopped. */
export interface TermRow {
  key: string;
  kind: "process" | "mcp";
  name: string;
  pid?: number;
  cpu_pct?: number;
  rss_kb?: number;
  running: boolean;
  tool?: ChatProcess;
}

/**
 * mk-60m.11: Activity's processes joined with the process tool's by name (a tool process's pid is its sandbox's).
 * A tool process Activity doesn't list (it exited, or Activity hasn't caught up) still shows, after the others.
 */
export function termRows(live: ChildProcess[], tool: ChatProcess[]): TermRow[] {
  const byName = new Map(tool.map((t) => [t.name, t]));
  const rows: TermRow[] = live.map((p) => {
    const t = p.kind === "process" ? byName.get(p.name) : undefined;
    if (t) byName.delete(p.name);
    const running = t ? t.state === "running" && p.alive !== false : p.alive !== false;
    // A tool process keeps one key whether or not Activity lists it yet, so its open log tail isn't remounted.
    return { key: t ? `tool:${t.name}` : `${p.kind}:${p.pid}`, kind: p.kind, name: p.name, pid: p.pid, cpu_pct: p.cpu_pct, rss_kb: p.rss_kb, running, tool: t };
  });
  for (const t of byName.values()) rows.push({ key: `tool:${t.name}`, kind: "process", name: t.name, running: t.state === "running", tool: t });
  return rows;
}

/** Only a forwarded port's own page opens from the dock: `http://localhost:<port>[/…]`, never another link a process
 * could have put in its name or notes. */
export function isProcessUrl(url: string): boolean {
  return /^http:\/\/localhost:\d{1,5}(\/|$)/.test(url);
}

/**
 * mk-60m.11: which replies still count, for a view that polls. Each look takes a token (`begin`); its replies apply only
 * while it's the latest look and the view is still showing the same chat (`end` on a chat switch or unmount), so a slow
 * reply about chat A never lands under chat B, nor an older reply over a newer one.
 */
export function replyGuard(): { begin: () => () => boolean; end: () => void } {
  let latest = 0;
  let ended = false;
  return {
    begin: () => {
      const mine = ++latest;
      return () => !ended && mine === latest;
    },
    end: () => {
      ended = true;
    },
  };
}

/** Whether a chat's agent just finished a turn (busy, then not): what it wrote is on disk now, so the files shown are
 * read again then instead of at the next look. */
export function turnEnded(busyBefore: boolean | undefined, busyNow: boolean | undefined): boolean {
  return busyBefore === true && busyNow === false;
}
