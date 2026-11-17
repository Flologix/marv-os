// One-step team setup (the New team dialog and the Team view): the pure pieces. What these read comes from the
// desktop's commands (list_teams, list_roles, live_chats, activity, write_overlaps, team_reports); nothing here decides
// anything a command didn't say. Only places.ts imported (no imports there), so the tests run under plain Node.
import { type LiveChat, type Tone, chatTone, isProcessUrl } from "./places.ts";

/** backend.ts's `RoleEntry`, as far as these read it. */
export interface RoleShape {
  name: string;
  description: string;
  tools: string[] | null;
  except: string[];
  writes: string[] | null;
  takesTasksFrom: string[];
}

/** backend.ts's `TeamEntry`, as far as these read it. */
export interface TemplateShape {
  name: string;
  members: { role: string; model: string | null }[];
  worktrees: boolean;
  readChats: boolean;
}

/** One member row of the New team dialog. */
export interface MemberDraft {
  role: string;
  title: string;
  /** "" or "inherit": the default model; "small": your small model; else a model id. */
  model: string;
  /** The write paths as typed ("styles.css, pages/"). */
  writes: string;
  /** What runs it (the harness plan, phase 3): unset or "" for Marv, "claude-code" for your own Claude Code. */
  harness?: string;
}

/** "styles.css, ./pages/  src" → ["styles.css", "./pages/", "src"]. */
export function parseWrites(text: string): string[] {
  return text
    .split(/[,\s]+/)
    .map((w) => w.trim())
    .filter(Boolean);
}

/** Whether the role may start processes (dev servers): it lists `process`, or has every tool but doesn't except it. */
function runsProcesses(role: RoleShape): boolean {
  return role.tools ? role.tools.includes("process") : !role.except.includes("process");
}

/**
 * The read-only bounds line of a member row: "writes styles.css, pages/ · can run dev servers · takes tasks from
 * coordinator". `writes`: what the person typed (it's what "Save as team file" writes); empty: the role's own.
 */
export function boundsSummary(role: RoleShape, writes = ""): string {
  const typed = parseWrites(writes);
  const parts: string[] = [];
  if (role.writes !== null && role.writes.length === 0) parts.push("changes no files");
  else if (typed.length) parts.push(`writes only ${typed.join(", ")}`);
  else if (role.writes) parts.push(`writes only ${role.writes.join(", ")}`);
  else parts.push("no write paths: edits on a teammate's task wait for your approval");
  if (runsProcesses(role)) parts.push("can run dev servers");
  parts.push(role.takesTasksFrom.length ? `takes tasks from ${role.takesTasksFrom.join(", ")}` : "takes tasks only from you");
  return parts.join(" · ");
}

/** Whether the role may be given write paths (a role that changes no files can't be widened). */
export function takesWrites(role: RoleShape): boolean {
  return !(role.writes !== null && role.writes.length === 0);
}

/** The editing members with no write paths: they ask before every edit on a teammate's task, and may edit the same
 * files as another. */
export function missingWrites(drafts: MemberDraft[], roles: RoleShape[]): MemberDraft[] {
  return drafts.filter((d) => {
    const role = roles.find((r) => r.name === d.role);
    return role !== undefined && role.writes === null && parseWrites(d.writes).length === 0;
  });
}

/** The write-path field's example for a role (a hint, never a value). */
export function writesHint(role: string): string {
  if (role === "frontend") return "e.g. index.html, styles.css, src/components/";
  if (role === "backend") return "e.g. server/, api/, tests/";
  return "folders or files this chat may write, e.g. src/, README.md";
}

/** A chat name from the folder: "my site" + "frontend" → "my-site-frontend", made unique among `taken`. */
export function chatName(folderName: string, role: string, taken: string[]): string {
  const slug = folderName.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "team";
  const base = `${slug}-${role}`;
  let name = base;
  for (let n = 2; taken.includes(name); n++) name = `${base}-${n}`;
  return name;
}

/** The member rows a template starts the dialog with: each role's write paths prefilled. */
export function membersFor(template: TemplateShape, folderName: string, roles: RoleShape[]): MemberDraft[] {
  const out: MemberDraft[] = [];
  for (const m of template.members) {
    const role = roles.find((r) => r.name === m.role);
    out.push({
      role: m.role,
      title: chatName(folderName, m.role, out.map((d) => d.title)),
      model: m.model ?? "",
      writes: role?.writes?.join(", ") ?? "",
    });
  }
  return out;
}

/** A new row (the dialog's "Add member"): a worker if there is one, else the first role. */
export function addMember(drafts: MemberDraft[], folderName: string, roles: RoleShape[]): MemberDraft[] {
  const role = roles.find((r) => r.name === "worker") ?? roles[0];
  if (!role) return drafts;
  return [...drafts, { role: role.name, title: chatName(folderName, role.name, drafts.map((d) => d.title)), model: "", writes: role.writes?.join(", ") ?? "" }];
}

/** A row whose role changed: its name follows the role (unless the person renamed it) and its write paths reset. */
export function changeRole(drafts: MemberDraft[], i: number, roleName: string, folderName: string, roles: RoleShape[]): MemberDraft[] {
  const role = roles.find((r) => r.name === roleName);
  return drafts.map((d, j) => {
    if (j !== i) return d;
    const others = drafts.filter((_, k) => k !== i).map((x) => x.title);
    const auto = d.title.startsWith(chatName(folderName, d.role, []));
    return { ...d, role: roleName, title: auto ? chatName(folderName, roleName, others) : d.title, writes: role?.writes?.join(", ") ?? "" };
  });
}

/** What the dialog sends to start_team / save_team_file (backend.ts's `TeamSpec`). */
export interface TeamSpecShape {
  name: string;
  description: string | null;
  members: { role: string; title: string; model: string | null; writes: string[] | null; harness?: string | null }[];
  worktrees: boolean;
  readChats: boolean;
  keepWorking: boolean;
  defaultModel: string;
  maxSteps: number;
  yolo: boolean;
}

export function specOf(
  name: string,
  drafts: MemberDraft[],
  how: { worktrees: boolean; readChats: boolean; keepWorking: boolean; description?: string | null },
  settings: { model: string; maxSteps: number; yolo: boolean },
): TeamSpecShape {
  return {
    name: name.trim(),
    description: how.description ?? null,
    members: drafts.map((d) => ({
      role: d.role,
      title: d.title.trim(),
      model: d.model.trim() || null,
      writes: d.writes.trim() ? parseWrites(d.writes) : null,
      ...(d.harness ? { harness: d.harness } : {}),
    })),
    worktrees: how.worktrees,
    readChats: how.readChats,
    keepWorking: how.keepWorking,
    defaultModel: settings.model,
    maxSteps: teamSteps(settings.maxSteps),
    yolo: settings.yolo,
  };
}

/**
 * A team member's step limit: at least 300. A member works on its lead's tasks without you watching, and a real task
 * (build a feature, fix its tests) takes well over a chat's usual 50 or 100 steps; each limit reached is a "Keep
 * going?" the person has to answer. Spend stays bounded by the grant's day cap and your budgets.
 */
export const TEAM_MAX_STEPS = 300;
export const teamSteps = (maxSteps: number) => Math.min(500, Math.max(maxSteps, TEAM_MAX_STEPS));

/** Why the dialog can't start yet (null: it can). The backend checks again. */
export function draftProblem(name: string, drafts: MemberDraft[]): string | null {
  if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(name.trim())) return "The team's name: lowercase letters, digits and dashes.";
  if (drafts.length === 0) return "Add a member.";
  if (drafts.length > 8) return "A team has at most 8 members.";
  const titles = drafts.map((d) => d.title.trim());
  if (titles.some((t) => !t)) return "Each chat needs a name.";
  if (new Set(titles).size !== titles.length) return "Two chats have the same name.";
  return null;
}

/**
 * The page a running process serves, when its name says a localhost port (`vite :5173`, `localhost:3000`): only
 * `http://localhost:<port>`, never another link a name could carry. Null when it doesn't say one.
 */
export function processUrl(name: string): string | null {
  const m = /(?:localhost|127\.0\.0\.1)?:(\d{2,5})\b/.exec(name);
  if (!m) return null;
  const port = Number(m[1]);
  if (port < 1 || port > 65535) return null;
  const url = `http://localhost:${port}`;
  return isProcessUrl(url) ? url : null;
}

/** backend.ts's `ChatRecord`, as far as the roster reads it. */
export interface ChatShape {
  id: string;
  title: string;
  folder: string;
  role?: string | null;
  team?: { name: string; roster: string[] } | null;
  settings?: { model: string } | null;
  /** The write paths it was given when its team started (enforced); none: its role's. */
  writes?: string[] | null;
}

/** backend.ts's `AgentRow`, as far as the roster reads it. */
export interface AgentShape {
  chat?: string;
  live: { core?: string; processes?: { kind: string; name: string; alive?: boolean }[] };
}

/** backend.ts's `WriteOverlap`, as far as the roster reads it. */
export interface OverlapShape {
  chat: string;
  with: { name: string; paths: string[] }[];
}

/** One member's card in the Team view. */
export interface RosterCard {
  id: string;
  title: string;
  role: string;
  tone: Tone;
  word: string;
  model: string;
  /** The write paths it was started with (enforced); null: its role's. */
  writes: string[] | null;
  /** Approvals waiting for you. */
  waiting: number;
  /** "styles.css (also: demo-backend)": files it writes that another chat writes too. */
  editing: string[];
  processes: { name: string; url: string | null }[];
  report: string | null;
}

/** A member's last report-back to the team's lead ("done: …", "failed: …", from team_reports). */
export function lastReport(member: { id: string }, replies: { chat: string; text: string }[]): string | null {
  return replies.find((r) => r.chat === member.id)?.text ?? null;
}

/** The Team view's roster: the team's chats in the order they started, each with its state and what it's doing. */
export function roster(
  ids: string[],
  chats: ChatShape[],
  live: LiveChat[],
  agents: AgentShape[],
  overlaps: OverlapShape[],
  replies: { chat: string; text: string }[],
): RosterCard[] {
  const cards: RosterCard[] = [];
  for (const id of ids) {
    const chat = chats.find((c) => c.id === id);
    if (!chat) continue;
    const state = live.find((l) => l.id === id);
    const { tone, word } = chatTone(state);
    const agent = agents.find((a) => a.chat === id);
    const overlap = overlaps.find((o) => o.chat === id);
    const editing = (overlap?.with ?? []).flatMap((w) => w.paths.map((p) => `${p} (also: ${w.name})`));
    const processes = (agent?.live.processes ?? [])
      .filter((p) => p.kind === "process" && p.alive !== false)
      .map((p) => ({ name: p.name, url: processUrl(p.name) }));
    cards.push({
      id,
      title: chat.title,
      role: chat.role ?? "",
      tone,
      word,
      model: agent?.live.core ?? chat.settings?.model ?? "",
      writes: chat.writes ?? null,
      waiting: state?.waiting ?? 0,
      editing,
      processes,
      report: lastReport({ id }, replies),
    });
  }
  return cards;
}

/** The coordinator's chat id, else the first member's: where "Tell the team" sends. */
export function teamLead(ids: string[], chats: ChatShape[]): string | null {
  return ids.find((id) => chats.find((c) => c.id === id)?.role === "coordinator") ?? ids[0] ?? null;
}

/** A folder's chats split for the sidebar: each team's members under its header (in roster order), then the rest. */
export function splitByTeam<C extends ChatShape>(chats: C[]): { teams: { name: string; chats: C[] }[]; rest: C[] } {
  const teams: { name: string; chats: C[] }[] = [];
  const rest: C[] = [];
  for (const chat of chats) {
    const name = chat.team?.name;
    if (!name) {
      rest.push(chat);
      continue;
    }
    let team = teams.find((t) => t.name === name);
    if (!team) teams.push((team = { name, chats: [] }));
    team.chats.push(chat);
  }
  for (const team of teams) {
    const order = team.chats[0]?.team?.roster ?? [];
    const rank = (c: C) => {
      const i = order.indexOf(c.role ?? "");
      return i < 0 ? order.length : i;
    };
    team.chats.sort((a, b) => rank(a) - rank(b));
  }
  return { teams, rest };
}

/** "Team web: coordinator, frontend, backend, devserver". */
export function teamHeader(name: string, chats: ChatShape[]): string {
  return `Team ${name}: ${chats.map((c) => c.role || c.title).join(", ")}`;
}

/** The lead's reply to the person's last message, as "Tell the team" shows it inline: the text after the last user
 * item (joined), or null while none came yet. */
export function replyAfterLastUser(items: { kind: string; text?: string }[]): string | null {
  let last = -1;
  items.forEach((it, i) => {
    if (it.kind === "user") last = i;
  });
  if (last < 0) return null;
  const text = items
    .slice(last + 1)
    .filter((it) => it.kind === "text" && it.text)
    .map((it) => it.text!.trim())
    .join("\n\n");
  return text || null;
}

/** The built-in roles in plain words, for the New team dialog: a label and what the chat does. */
const FRIENDLY: Record<string, { label: string; does: string }> = {
  coordinator: { label: "Lead", does: "Plans the work, hands tasks to the others and reports back to you." },
  worker: { label: "Builder", does: "Does the tasks it's given: writes code, edits files, runs commands." },
  reviewer: { label: "Reviewer", does: "Checks the others' work and points out problems. Changes no files." },
  frontend: { label: "Frontend", does: "Builds the pages, styles and components people see." },
  backend: { label: "Backend", does: "Builds the server, API, data and tests." },
  devserver: { label: "Dev server", does: "Keeps the app running so you can open it in your browser." },
};

/** A role's name as the dialog shows it: "Lead" for coordinator; a role of the folder's own, as named. */
export function roleLabel(role: string): string {
  return FRIENDLY[role]?.label ?? role.charAt(0).toUpperCase() + role.slice(1);
}

/** What a role's chat does, in a sentence: the built-in's plain words, else the role file's description. */
export function roleDoes(role: { name: string; description: string }): string {
  return FRIENDLY[role.name]?.does ?? role.description;
}

/** "Start from scratch": a lead and a builder, the smallest team that hands out work. */
export function scratchTemplate(roles: RoleShape[]): TemplateShape & { name: string } {
  const has = (r: string) => roles.some((x) => x.name === r);
  const members = (["coordinator", "worker"] as const).filter(has).map((role) => ({ role, model: null }));
  return { name: "team", members: members.length ? members : roles.slice(0, 1).map((r) => ({ role: r.name, model: null })), worktrees: false, readChats: true };
}
