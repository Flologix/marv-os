import { useCallback, useEffect, useState } from "react";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  type ActivityView,
  type AgentUsage,
  type AgentRow,
  type Away,
  type ChatRecord,
  type ChatGrants,
  type ChatPermissions,
  type MeshRules,
  desktop,
  grantSpend,
} from "../lib/backend";
import { say } from "../lib/dialog";
import { harnessTitle } from "../lib/harness";

/** Activity's state words (runtime-kernel §20.1). */
export function stateWord(row: AgentRow): string {
  if (row.live.paused) return "paused";
  switch (row.state) {
    case "sleeping":
      return "sleeping";
    case "loading":
      return "loading";
    case "idle":
      return "idle";
    case "busy":
      return "busy";
    case "waiting":
      return "waiting for you";
    default:
      return "running";
  }
}


const dollars = (micros?: number) => (micros === undefined ? "—" : `$${(micros / 1e6).toFixed(2)}`);

const memory = (kb?: number) => (kb === undefined ? "—" : kb >= 1024 * 1024 ? `${(kb / 1024 / 1024).toFixed(1)} GB` : kb >= 1024 ? `${Math.round(kb / 1024)} MB` : `${kb} KB`);

/** A folder as the person knows it: `~/Projects/web`, not the full path. */
const shortFolder = (folder: string) =>
  folder
    .replace(/^\/home\/[^/]+(?=\/|$)/, "~")
    .replace(/^\/Users\/[^/]+(?=\/|$)/, "~")
    .replace(/^[A-Za-z]:\\Users\\[^\\]+(?=\\|$)/i, "~");


/**
 * The desktop chat behind a row: its own (a chat's door), or the chat whose mesh name a terminal Marv took (a chat's
 * Marv runs in its terminal under the chat's name, `<folder>-<first 8 of its id>`). For its name and agent only: the
 * row's signals stay with the rows the desktop vouches for (`row.chat`).
 */
function chatOf(row: AgentRow, chats: ChatRecord[]): ChatRecord | undefined {
  if (row.chat) return chats.find((c) => c.id === row.chat);
  return chats.find((c) => row.name.endsWith(`-${c.id.slice(0, 8)}`));
}


/** One agent's row: its chat and agent, its state, its process's CPU and memory, today's spend, and the person's signals. */
function Row({ row, chat: record, onOpen, onDone }: { row: AgentRow; chat?: ChatRecord; onOpen: (chat: string) => void; onDone: () => void }) {
  const chat = row.chat;
  const opens = chat ?? record?.id;
  async function signal(kind: "pause" | "resume" | "stop") {
    if (!chat) return;
    try {
      await desktop.chatSignal(chat, kind);
    } catch (e) {
      void say(String(e), { title: "Marv", kind: "error" });
    }
    onDone();
  }
  // Its real process (a terminal agent's, with everything it runs), measured by the operating system.
  const procs = (row.live.processes ?? []).filter((p) => p.alive !== false);
  const cpu = procs.some((p) => p.cpu_pct !== undefined) ? `${procs.reduce((n, p) => n + (p.cpu_pct ?? 0), 0)}%` : "—";
  const rss = procs.some((p) => p.rss_kb !== undefined) ? memory(procs.reduce((n, p) => n + (p.rss_kb ?? 0), 0)) : "—";
  const agent = record ? harnessTitle(record.harness) : row.live.core ?? "";
  return (
    <tr className={`act-row ${row.live.paused ? "paused" : row.state ?? ""}`}>
      <td className="act-name" title={`${row.name} · pid ${row.pid}`}>
        {record?.title || row.title || row.name}
        {agent && <span className="act-kind">{agent}</span>}
      </td>
      <td>{stateWord(row)}</td>
      <td>{cpu}</td>
      <td>{rss}</td>
      <td title="Spent today">{dollars(row.live.spend_usd_micros)}</td>
      <td className="act-actions">
        {opens && (
          <button className="icon-btn" title="Open the chat" onClick={() => onOpen(opens)}>
            ↗
          </button>
        )}
        {chat &&
          (row.live.paused ? (
            <button className="icon-btn" title="Resume: mail and timers may wake it again" onClick={() => void signal("resume")}>
              ▶
            </button>
          ) : (
            <button className="icon-btn" title="Pause: its turn ends after this step; mail and timers wait" onClick={() => void signal("pause")}>
              ❙❙
            </button>
          ))}
        {chat && (
          <button className="icon-btn danger" title="Stop its turn now" onClick={() => void signal("stop")}>
            ■
          </button>
        )}
      </td>
    </tr>
  );
}

const tokens = (n: number) => (n >= 1e9 ? `${(n / 1e9).toFixed(1)}B` : n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}k` : `${n}`);

/**
 * What the agents spent, from their own history on this computer: Claude Code's, Codex's and OpenCode's, in Marv OS
 * or in another terminal. Its cost is the same tokens at list prices, so it says what a subscription saves, not what it
 * bills.
 */
export function AgentUsagePanel() {
  const [days, setDays] = useState(7);
  const [usage, setUsage] = useState<AgentUsage | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    const load = () =>
      desktop.agentUsage(days).then(
        (u) => live && (setUsage(u), setError(null)),
        (e) => live && setError(String(e)),
      );
    void load();
    const timer = setInterval(load, 60_000);
    return () => ((live = false), clearInterval(timer));
  }, [days]);
  const lines = usage?.lines ?? [];
  const total = lines.reduce((n, l) => n + (l.usdMicros ?? 0), 0);
  const peak = Math.max(1, ...(usage?.days ?? []).map(([, u]) => u));
  return (
    <div className="act-section agent-usage">
      <div className="act-head">
        <h3>Agents' usage</h3>
        <span className="faint">from each agent's own history, in Marv OS or not · at list prices, not your plan's bill</span>
        <span className="segmented act-total" role="radiogroup" aria-label="Days">
          {[1, 7, 30].map((d) => (
            <button key={d} role="radio" aria-checked={days === d} className={days === d ? "on" : ""} onClick={() => setDays(d)}>
              {d === 1 ? "today" : `${d} days`}
            </button>
          ))}
        </span>
      </div>
      {error && <p className="error">{error}</p>}
      {usage?.problems.map((p) => (
        <p key={p} className="faint">
          {p}
        </p>
      ))}
      {!usage && !error && <p className="faint">Reading the agents' history…</p>}
      {usage && lines.length === 0 && <p className="faint">No Claude Code, Codex or OpenCode work in these days.</p>}
      {usage && usage.days.length > 1 && (
        <div className="usage-days" aria-label="Cost by day">
          {usage.days.map(([day, micros]) => (
            <span key={day} title={`${new Date(day).toLocaleDateString()}: ${dollars(micros)}`} style={{ height: `${Math.max(2, (micros / peak) * 100)}%` }} />
          ))}
        </div>
      )}
      {lines.length > 0 && (
        <table className="act-table">
          <thead>
            <tr>
              <th>Agent</th>
              <th>Model</th>
              <th>Requests</th>
              <th title="Input not read from the cache">Input</th>
              <th>Output</th>
              <th title="Input read from the cache, and the share of all input it was">Cache read</th>
              <th>Cache write</th>
              <th title="The same tokens at the provider's list prices">Cost</th>
            </tr>
          </thead>
          <tbody>
            {lines.map((l) => {
              const all = l.input + l.cacheRead + l.cacheWrite;
              return (
                <tr key={`${l.agent} ${l.model}`}>
                  <td>{harnessTitle(l.agent)}</td>
                  <td>{l.model}</td>
                  <td>{l.requests}</td>
                  <td>{tokens(l.input)}</td>
                  <td>{tokens(l.output)}</td>
                  <td>
                    {tokens(l.cacheRead)}
                    {all > 0 && <span className="faint"> {Math.round((l.cacheRead / all) * 100)}%</span>}
                  </td>
                  <td>{tokens(l.cacheWrite)}</td>
                  <td title={l.usdMicros === null ? "No list price known for this model" : l.partlyPriced ? "Some requests had no known price" : undefined}>
                    {l.usdMicros === null ? "—" : `${dollars(l.usdMicros)}${l.partlyPriced ? "+" : ""}`}
                  </td>
                </tr>
              );
            })}
            <tr className="act-group-head">
              <td colSpan={7}>Total</td>
              <td>{dollars(total)}</td>
            </tr>
          </tbody>
        </table>
      )}
    </div>
  );
}

/**
 * Activity (runtime-kernel §20.1, O1): every agent on this computer, by folder, from the kernel (their doors), never
 * this page's guess: what each is doing, what its process uses, what it spent today, and the person's signals. For
 * the person's eyes only: nothing here is offered to a model. (About moved to Settings; the kernel's log and the
 * notification list are gone from the person's view: names, hops and a second copy of the notifications.)
 */
export function Activity({
  onOpen,
  chats,
  todayUsd,
}: {
  onOpen: (chat: string) => void;
  chats: ChatRecord[];
  /** Today's spend, as the top bar says it (one total, not two that disagree). */
  todayUsd: number | null;
}) {
  const [view, setView] = useState<ActivityView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const refresh = useCallback(() => {
    desktop.activity().then(
      (v) => (setView(v), setError(null)),
      (e) => setError(String(e)),
    );
  }, []);
  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, 2000);
    return () => clearInterval(timer);
  }, [refresh]);

  const rows = view?.activity.rows ?? [];
  const folders = [...new Set(rows.map((r) => r.folder))];
  return (
    <section className="activity">
      <div className="act-head">
        <h2>Activity</h2>
        <span className="faint">every agent on this computer</span>
        {todayUsd !== null && <span className="act-total">today ${todayUsd.toFixed(2)}</span>}
      </div>
      {error && <p className="error">{error}</p>}
      {!view && !error && <p className="faint">Looking…</p>}
      {view && rows.length === 0 && <p className="faint">No agent is running here.</p>}
      {rows.length > 0 && (
        <table className="act-table">
          <thead>
            <tr>
              <th>Chat</th>
              <th>Status</th>
              <th title="Its process and everything it runs: share of one core">CPU</th>
              <th>Memory</th>
              <th>Today</th>
              <th />
            </tr>
          </thead>
          {folders.map((folder) => (
            <tbody key={folder}>
              <tr className="act-group-head">
                <td colSpan={6} title={folder}>
                  {shortFolder(folder)}
                </td>
              </tr>
              {rows
                .filter((r) => r.folder === folder)
                .map((row) => (
                  <Row key={row.name} row={row} chat={chatOf(row, chats)} onOpen={onOpen} onDone={refresh} />
                ))}
            </tbody>
          ))}
        </table>
      )}
      <AgentUsagePanel />
    </section>
  );
}


/**
 * Permissions (runtime-kernel §20.4, O4): who can do what, by chat or by rule and grant. Show and revoke only: a
 * permission is made where it always was (the rule dialog, a y/N in the terminal).
 */
export function Permissions() {
  const [lens, setLens] = useState<"chat" | "rule">("chat");
  const [chats, setChats] = useState<ChatPermissions[]>([]);
  const [rules, setRules] = useState<MeshRules | null>(null);
  const [grants, setGrants] = useState<ChatGrants | null>(null);
  const load = useCallback(() => {
    desktop.permissions().then(setChats, () => setChats([]));
    desktop.meshRules().then(setRules, () => setRules(null));
    desktop.chatGrants().then(setGrants, () => setGrants(null));
  }, []);
  useEffect(() => {
    load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  }, [load]);

  async function revoke(what: { kind: "rule" | "grant" | "timer"; id: number }, text: string) {
    if (!(await ask(`Revoke this? ${text}`, { title: "Marv", kind: "warning" }))) return;
    try {
      if (what.kind === "rule") await desktop.revokeRule(what.id);
      else if (what.kind === "grant") await desktop.revokeGrant(what.id);
      else await desktop.removeTimer(what.id);
    } catch (e) {
      void say(String(e), { title: "Marv", kind: "error" });
    }
    load();
  }

  return (
    <section className="perms">
      <div className="perm-head">
        <span className="muted">who can do what: revoke here, make permissions where you always have</span>
        <span className="segmented" role="radiogroup" aria-label="Group by">
          <button role="radio" aria-checked={lens === "chat"} className={lens === "chat" ? "on" : ""} onClick={() => setLens("chat")}>
            by chat
          </button>
          <button role="radio" aria-checked={lens === "rule"} className={lens === "rule" ? "on" : ""} onClick={() => setLens("rule")}>
            by rule or grant
          </button>
        </span>
      </div>
      {lens === "chat" ? (
        <ul className="act-log">
          {chats.length === 0 && <p className="faint">No chats yet.</p>}
          {chats.map((c) => (
            <li key={c.chat}>
              <b>{c.name}</b>{" "}
              {c.may.length === 0 ? (
                <span className="faint">asks you first for everything</span>
              ) : (
                <>
                  may:{" "}
                  {c.may.map((p, i) => (
                    <span key={i}>
                      {i > 0 && " · "}
                      {p.text}{" "}
                      <button className="icon-btn danger" title="Revoke" onClick={() => void revoke(p.revoke, p.text)}>
                        ×
                      </button>
                    </span>
                  ))}
                </>
              )}
            </li>
          ))}
        </ul>
      ) : (
        <>
          <div className="act-section">
            <h3>Rules</h3>
            {rules?.problem && <p className="error">{rules.problem}</p>}
            {(rules?.rules ?? []).length === 0 && <p className="faint">none: chats ask you before messaging or reading each other</p>}
            <ul className="act-log">
              {(rules?.rules ?? []).map((r) => (
                <li key={r.id}>
                  rule {r.id}: {r.from} {r.mutual ? "⇄" : "→"} {r.to} · {r.mode === "read" ? "may read chats" : "may message"} ·{" "}
                  {r.untilQuit ? "until Marv quits" : r.expiresAtMs === null ? "until revoked" : `until ${new Date(r.expiresAtMs).toLocaleString()}`}{" "}
                  <button className="icon-btn danger" title="Revoke" onClick={() => void revoke({ kind: "rule", id: r.id }, `rule ${r.id}`)}>
                    ×
                  </button>
                </li>
              ))}
            </ul>
          </div>
          <div className="act-section">
            <h3>Grants: work while you're away</h3>
            {grants?.problem && <p className="error">{grants.problem}</p>}
            {(grants?.grants ?? []).length === 0 && <p className="faint">none</p>}
            <ul className="act-log">
              {(grants?.grants ?? []).map((g) => (
                <li key={g.id}>
                  grant {g.id}: {g.title || g.folder} · {grantSpend(g)} ·{" "}
                  {g.untilQuit ? "until Marv quits" : g.expiresAtMs === null ? "until revoked" : `until ${new Date(g.expiresAtMs).toLocaleString()}`}{" "}
                  <button className="icon-btn danger" title="Revoke" onClick={() => void revoke({ kind: "grant", id: g.id }, `grant ${g.id}`)}>
                    ×
                  </button>
                  {g.timers.map((t) => (
                    <div key={t.id} className="faint">
                      timer {t.id}: {new Date(t.atMs).toLocaleString()}
                      {t.everyMinutes ? `, every ${t.everyMinutes} min` : ""}{" "}
                      <button className="icon-btn danger" title="Remove the timer" onClick={() => void revoke({ kind: "timer", id: t.id }, `timer ${t.id}`)}>
                        ×
                      </button>
                    </div>
                  ))}
                </li>
              ))}
            </ul>
          </div>
        </>
      )}
    </section>
  );
}

/** The "while you were away" card's fixed words (§20.3): "3 chats ran 41 calls · 1 needs you · $0.62 spent". */
export function awayText(a: Away): string {
  const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);
  const parts = [`${a.chats} ${plural(a.chats, "chat", "chats")} ran ${a.calls} ${plural(a.calls, "call", "calls")}`];
  if (a.needs_you > 0) parts.push(`${a.needs_you} ${plural(a.needs_you, "needs", "need")} you`);
  if (a.paused > 0) parts.push(`${a.paused} paused at ${plural(a.paused, "its", "their")} cap`);
  if (a.spent_usd_micros !== undefined) parts.push(`$${(a.spent_usd_micros / 1e6).toFixed(2)} spent`);
  return parts.join(" · ");
}
