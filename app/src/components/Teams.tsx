import { ask } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useState } from "react";
import {
  type ActivityView,
  type ChatGrant,
  type ChatGrants,
  type ChatRecord,
  type ChatSettings,
  type LiveChat,
  type MarvConfig,
  type MeshRule,
  type MeshRules,
  type Model,
  type TeamList,
  type WriteOverlap,
  desktop,
  grantSpend,
} from "../lib/backend";
import type { Item } from "../lib/chat";
import { Board } from "./Board";
import { NewTeam, type TeamPick } from "./NewTeam";
import { roleLabel } from "../lib/teams";
import { TeamView } from "./TeamView";

const LASTS: { value: string; label: string }[] = [
  { value: "revoked", label: "until I revoke it" },
  { value: "hour", label: "for 1 hour" },
  { value: "quit", label: "until Marv quits" },
];

/** A rule's folder by its name (`~/Projects/acme` → `acme`), a chat as "a chat". */
export function base(subject: string) {
  return subject.startsWith("chat ") ? "a chat" : subject.split(/[\\/]/).filter(Boolean).pop() ?? subject;
}

export function ruleName(rule: MeshRule) {
  return rule.label || (rule.from === rule.to ? base(rule.from) : `${base(rule.from)} → ${base(rule.to)}`);
}

function lasting(rule: { untilQuit: boolean; expiresAtMs: number | null }) {
  if (rule.untilQuit) return "until Marv quits";
  if (rule.expiresAtMs === null) return "until revoked";
  const left = rule.expiresAtMs - Date.now();
  if (left <= 0) return "expired";
  const minutes = Math.ceil(left / 60_000);
  return minutes >= 60 ? `${Math.round(minutes / 60)} h left` : `${minutes} min left`;
}

/** A timer's period in whole units: "day", "2 days", "3 h", "90 min" (never "1.5 day"). */
function period(minutes: number): string {
  const unit = (n: number, one: string, many: string) => (n === 1 ? one : `${n} ${many}`);
  if (minutes % 1440 === 0) return unit(minutes / 1440, "day", "days");
  if (minutes % 60 === 0) return unit(minutes / 60, "hour", "hours");
  return `${minutes} min`;
}

/**
 * Kernel #53, Teams (what the Mesh panel was): your rules (runtime-kernel 2b: which chats may message each other
 * without asking; made only here by your click, or `marv mesh allow`), stopping and resuming a rule's team, the chats
 * that keep working while you're away, chats editing the same files, the mesh's own switches, and the selected rule's
 * board.
 */
export function Teams({
  folder,
  folderName,
  newTeam,
  onNewTeam,
  settings,
  models,
  hasKey,
  chats,
  live: liveChats,
  open,
  onOpenChat,
  onChatsChanged,
}: {
  folder: string;
  folderName: string;
  /** The New team dialog is open (from here, or a folder's header in Folders). */
  newTeam: boolean;
  onNewTeam: (open: boolean) => void;
  settings: ChatSettings;
  models: Model[];
  hasKey: boolean;
  chats: ChatRecord[];
  live: LiveChat[];
  open: { id: string | null; send: (text: string) => void; items: Item[] };
  onOpenChat: (id: string) => void;
  onChatsChanged: () => void;
}) {
  const [teamList, setTeamList] = useState<TeamList | null>(null);
  const [state, setState] = useState<MeshRules | null>(null);
  const [grants, setGrants] = useState<ChatGrants | null>(null);
  const [overlaps, setOverlaps] = useState<WriteOverlap[]>([]);
  const [teams, setTeams] = useState<ActivityView["teams"]>([]);
  const [config, setConfig] = useState<MarvConfig | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // The template the New team dialog opens on (a card here), and each new team's first task, for its lead.
  const [initial, setInitial] = useState<TeamPick | undefined>(undefined);
  const [tasks, setTasks] = useState<Record<string, string>>({});
  const openNew = (pick?: TeamPick) => (setInitial(pick), onNewTeam(true));

  const refresh = useCallback(() => {
    void desktop.meshRules().then(setState, (e) => setError(String(e)));
    void desktop.chatGrants().then(setGrants, () => setGrants(null));
    void desktop.writeOverlaps().then(setOverlaps, () => setOverlaps([]));
    void desktop.activity().then((v) => setTeams(v.teams), () => setTeams([]));
    void desktop.listTeams(folder).then(setTeamList, () => setTeamList(null));
  }, [folder]);
  useEffect(() => {
    refresh();
    void desktop.marvConfig().then(setConfig, () => setConfig(null));
    const timer = setInterval(refresh, 5000);
    return () => clearInterval(timer);
  }, [refresh]);

  const rules = state?.rules ?? [];
  const live = rules.filter((r) => r.mode !== "watch");
  const rule = rules.find((r) => r.id === selected) ?? live[0] ?? rules[0];

  async function guard(action: () => Promise<unknown>) {
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(String(e));
    }
    refresh();
  }

  async function revoke(r: MeshRule) {
    if (!(await ask(`Revoke "${ruleName(r)}"? Chats it covered ask again before messaging each other.`, { title: "Marv", kind: "warning" }))) return;
    await guard(async () => setState(await desktop.revokeRule(r.id)));
  }

  async function stopTeam(r: MeshRule) {
    if (!(await ask(`Stop every chat "${ruleName(r)}" covers? Their turns stop, and they stay paused until you resume each one.`, { title: "Marv", kind: "warning" })))
      return;
    await guard(async () => setNotice(`${await desktop.stopTeam(r.id)} stopped.`));
  }

  async function setMesh(patch: Parameters<typeof desktop.setMarvConfig>[0]) {
    try {
      setConfig(await desktop.setMarvConfig(patch));
    } catch (e) {
      setError(String(e));
    }
  }

  const covered = (r: MeshRule) => teams.find((t) => t.rule === r.id)?.names.length ?? 0;

  const here = teamList?.here ?? [];
  const rulesGrid = (
      <div className="place-grid">
        <section>
          <h2 className="place-h">Rules</h2>
          {rules.length === 0 && <p className="faint place-empty">None: chats ask you before messaging each other.</p>}
          {rules.map((r) => (
            <button
              key={r.id}
              className={`place-row rule${rule?.id === r.id ? " selected" : ""}`}
              title={`${r.from} ${r.mutual ? "⇄" : "→"} ${r.to} · made in the ${r.by}`}
              onClick={() => setSelected(r.id)}
            >
              <b>{ruleName(r)}</b>
              <span className="place-meta">
                {r.mode === "read" ? "read" : r.mode} · {r.mutual ? "mutual" : "one way"} · {covered(r)} chat{covered(r) === 1 ? "" : "s"} · {lasting(r)}
              </span>
            </button>
          ))}
          {rule && (
            <div className="place-buttons">
              <button className="btn" onClick={() => void stopTeam(rule)} title="Pause and stop every chat this rule covers">
                Stop the team
              </button>
              <button className="btn" onClick={() => void guard(async () => setNotice(`${await desktop.resumeTeam(rule.id)} resumed.`))} title="Every paused chat this rule covers runs again">
                Resume
              </button>
              <button className="btn plain" onClick={() => void revoke(rule)}>
                Revoke
              </button>
            </div>
          )}
          <MakeRule folder={folder} folderName={folderName} onMade={(made) => (setState(made), setNotice(made.notice ?? null), refresh())} />
          <Background grants={grants} onGrants={setGrants} onError={setError} />
          {overlaps.length > 0 && (
            <>
              <h2 className="place-h spaced">Editing the same files</h2>
              {overlaps.map((o) => (
                <div key={o.chat} className="place-row" title={`Written by both in the last 30 minutes:\n${o.with.map((w) => `${w.name}: ${w.paths.join(", ")}`).join("\n")}`}>
                  <b>{o.title || "a chat"}</b>
                  <span className="place-meta">
                    also editing here: {o.with.map((w) => `${w.name} (${w.paths.length} file${w.paths.length === 1 ? "" : "s"})`).join(", ")}
                  </span>
                </div>
              ))}
            </>
          )}
          <h2 className="place-h spaced">Mesh</h2>
          {config && (
            <>
              <label className="place-row setting">
                <span>
                  <b>Wake on mail</b>
                  <span className="place-meta">whose messages start a turn in an idle chat</span>
                </span>
                <select value={config.meshWake === null ? "rules" : config.meshWake ? "on" : "off"} onChange={(e) => void setMesh({ meshWake: e.target.value as "on" | "off" | "rules" })}>
                  <option value="rules">a rule's wires (default)</option>
                  <option value="on">any wire</option>
                  <option value="off">never</option>
                </select>
              </label>
              <Switch label="Radio" help="this Marv shows on the mesh radio (presence, with titles)" on={config.meshRadio} onChange={(on) => void setMesh({ meshRadio: on })} />
              <Switch label="Chat cards" help="a rule's chats see each other's card (model, folder, what it's on)" on={config.meshCards} onChange={(on) => void setMesh({ meshCards: on })} />
              {config.problem && <p className="notice error">Your Marv settings file isn't used: {config.problem}</p>}
            </>
          )}
          {state?.problem && <p className="notice error">Your rules file isn't used: {state.problem}</p>}
          {grants?.problem && <p className="notice error">Your grants file isn't used: {grants.problem}</p>}
          {notice && <p className="notice">{notice}</p>}
          {error && <p className="notice error">{error}</p>}
        </section>
        <section>
          <h2 className="place-h">Board{rule ? ` · ${ruleName(rule)}` : ""}</h2>
          {rule && rule.mode !== "watch" ? <Board rules={[rule.id]} /> : <p className="faint place-empty">No rules, so no boards: a rule's chats share one.</p>}
        </section>
      </div>
  );

  return (
    <main className="place teams" aria-label="Teams">
      <div className="teams-head">
        <div>
          <h1 className="place-title">Teams in {folderName}</h1>
          <span className="place-meta">Several chats working on {folderName} together. Pick another folder in Folders to make a team there.</span>
        </div>
        <button className="btn primary" onClick={() => openNew()} title={`Start chats in ${folderName} that work together, each with its role`}>
          New team
        </button>
      </div>
      {here.length === 0 && (
        <section className="teams-start">
          <p className="muted">
            Give Marv a bigger job and let a team split it up: a <b>lead</b> plans and hands out tasks, <b>helpers</b> do them in parallel and report
            back, and you follow along here. Pick a starting point:
          </p>
          <div className="team-cards">
            {(teamList?.teams ?? []).map((t) => (
              <button key={t.name} className="team-card" onClick={() => openNew(t.name)} title={t.description}>
                <b>{t.title}</b>
                <span className="small">{t.description}</span>
                <span className="member-pills">
                  {t.members.map((m, i) => (
                    <span key={i} className="member-pill">
                      {roleLabel(m.role)}
                    </span>
                  ))}
                </span>
              </button>
            ))}
            <button className="team-card scratch" onClick={() => openNew("scratch")}>
              <b>Start from scratch</b>
              <span className="small">A lead and a builder. Add whoever else you need.</span>
            </button>
          </div>
        </section>
      )}
      {here.map((team) => (
        <TeamView
          key={team.name}
          team={team}
          folder={folder}
          chats={chats}
          live={liveChats}
          settings={settings}
          open={open}
          onOpenChat={onOpenChat}
          onEnded={() => (refresh(), onChatsChanged())}
          firstTask={tasks[team.name]}
          onFirstTaskSent={() => setTasks(({ [team.name]: _, ...rest }) => rest)}
        />
      ))}
      <details className="teams-advanced">
        <summary>Advanced: rules, background work and mesh settings</summary>
        {rulesGrid}
      </details>
      {newTeam && (
        <NewTeam
          folder={folder}
          folderName={folderName}
          settings={settings}
          models={models}
          hasKey={hasKey}
          initial={initial}
          onClose={() => (setInitial(undefined), onNewTeam(false))}
          onStarted={(_, name, task) => {
            if (task) setTasks((t) => ({ ...t, [name]: task }));
            refresh();
            onChatsChanged();
            // The team's card shows it starting: the dialog goes (left open, a refresh reset it to "Set up a team",
            // over the running team).
            setInitial(undefined);
            onNewTeam(false);
          }}
        />
      )}
    </main>
  );
}

function Switch({ label, help, on, onChange }: { label: string; help: string; on: boolean; onChange: (on: boolean) => void }) {
  return (
    <div className="place-row setting">
      <span>
        <b>{label}</b>
        <span className="place-meta">{help}</span>
      </span>
      <button type="button" role="switch" aria-checked={on} aria-label={label} className={`toggle${on ? " on" : ""}`} onClick={() => onChange(!on)} />
    </div>
  );
}

/** Your click: chats under a folder may message each other (runtime-kernel 2b), optionally read each other's chats,
 * keep working while you're away (a grant each), each in its own worktree (§14.1). */
function MakeRule({ folder, folderName, onMade }: { folder: string; folderName: string; onMade: (made: MeshRules) => void }) {
  const [open, setOpen] = useState(false);
  const [lasts, setLasts] = useState("revoked");
  const [keepWorking, setKeepWorking] = useState(false);
  const [worktrees, setWorktrees] = useState(true);
  const [read, setRead] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function allow() {
    setError(null);
    setWorking(true);
    try {
      onMade(await desktop.allowFolder(folder, lasts, keepWorking, read, worktrees));
      setRead(false);
      setOpen(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }

  if (!open)
    return (
      <button className="add-link make-rule" onClick={() => setOpen(true)}>
        + Let chats under {folderName} work together
      </button>
    );
  return (
    <div className="add-server make-rule">
      <p className="help">
        Let chats under <b>{folderName}</b> (and its subfolders) message each other. Pick another folder in Folders first to make a rule there.
      </p>
      <label className="help" title="Under this rule a chat may read another's conversation (what you typed and what its assistant replied, never what a tool read or ran). A chat you mark private refuses.">
        <input type="checkbox" checked={read} onChange={(e) => setRead(e.target.checked)} /> …and may read each other's chats
      </label>
      <select value={lasts} onChange={(e) => setLasts(e.target.value)}>
        {LASTS.map((l) => (
          <option key={l.value} value={l.value}>
            {l.label}
          </option>
        ))}
      </select>
      <label className="help" title="Each chat there now gets its own grant (later chats ask for theirs): a message on a rule's wire may then edit files in its folder and run sandboxed commands without network, as yolo would, within $2 a day. Everything else still waits for you.">
        <input type="checkbox" checked={keepWorking} onChange={(e) => setKeepWorking(e.target.checked)} /> …and keep working while I'm away
      </label>
      {keepWorking && (
        <label className="help" title="Where two or more of the chats granted here share a folder, each new one starts in its own git worktree (a branch of its own), so they never write over each other; you merge the branches.">
          <input type="checkbox" checked={worktrees} onChange={(e) => setWorktrees(e.target.checked)} /> …each in its own worktree
        </label>
      )}
      <div className="add-buttons">
        <button className="btn primary" disabled={working} onClick={() => void allow()}>
          {working ? "Allowing…" : "Allow"}
        </button>
        <button className="btn plain" onClick={() => setOpen(false)}>
          Cancel
        </button>
      </div>
      {error && <p className="notice error">{error}</p>}
    </div>
  );
}

/** The chats that keep working while you're away (their grants): signals, timers, the day's cap, revoke. */
function Background({ grants, onGrants, onError }: { grants: ChatGrants | null; onGrants: (g: ChatGrants) => void; onError: (e: string) => void }) {
  const [timerFor, setTimerFor] = useState<string | null>(null);
  const [timerNote, setTimerNote] = useState("");
  const [timerWhen, setTimerWhen] = useState("morning");
  const list = grants?.grants ?? [];

  async function run(action: () => Promise<ChatGrants>) {
    try {
      onGrants(await action());
    } catch (e) {
      onError(String(e));
    }
  }

  async function revoke(grant: ChatGrant) {
    if (!(await ask(`Stop "${grant.title || "this chat"}" working while you're away? It asks you for everything again from its next call.`, { title: "Marv", kind: "warning" })))
      return;
    await run(() => desktop.revokeGrant(grant.id));
  }

  async function addTimer(grant: ChatGrant) {
    const now = Date.now();
    await run(async () => {
      if (timerWhen === "morning") {
        // At 9:00 in your zone, every day, across DST (Marv works out each next time).
        const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
        return desktop.addTimer(grant.chat, now, null, timerNote, { minute: 9 * 60, tz });
      }
      return desktop.addTimer(grant.chat, now + 60 * 60 * 1000, timerWhen === "hourly" ? 60 : null, timerNote);
    });
    setTimerFor(null);
    setTimerNote("");
  }

  return (
    <>
      <h2 className="place-h spaced">Working while you're away</h2>
      {list.length === 0 && <p className="faint place-empty">None: a chat a message wakes asks you before every change.</p>}
      {list.map((grant) => (
        <div key={grant.id} className="place-row grant" title={`${grant.folder} · runs what yolo would, nothing past it · ${grant.lastRunMs ? `last ran ${new Date(grant.lastRunMs).toLocaleTimeString()}` : "hasn't run yet"}`}>
          <div className="grant-line">
            <span className={`tone-dot ${grant.paused ? "waiting" : grant.held ? "paused" : "idle"}`} aria-hidden="true" />
            <b>{grant.title || base(grant.folder)}</b>
            <span className="place-meta grow">
              {grant.paused ? "paused · " : grant.held ? "held · " : ""}
              {grantSpend(grant)} · {lasting(grant)}
            </span>
            <span className="row-actions">
              {grant.held ? (
                <button className="icon-btn" title="Resume: mail and timers may wake it again" onClick={() => void run(() => desktop.chatSignal(grant.chat, "resume"))}>
                  ▶
                </button>
              ) : (
                <button className="icon-btn" title="Pause: its turn ends after this step; mail and timers wait" onClick={() => void run(() => desktop.chatSignal(grant.chat, "pause"))}>
                  ❚❚
                </button>
              )}
              <button className="icon-btn" title="Stop its turn now" onClick={() => void run(() => desktop.chatSignal(grant.chat, "stop"))}>
                ■
              </button>
              <button className="icon-btn" title="Add a timer" onClick={() => setTimerFor(timerFor === grant.chat ? null : grant.chat)}>
                ⏲
              </button>
              <button className="icon-btn" title="Raise today's cap by $2" onClick={() => void run(() => desktop.setGrantCap(grant.id, grant.capUsd + 2))}>
                +$
              </button>
              <button className="icon-btn danger" title="Revoke" onClick={() => void revoke(grant)}>
                ×
              </button>
            </span>
          </div>
          {grant.timers.map((t) => (
            <div key={t.id} className="grant-timer" title={t.by === "chat" ? "Set by the chat itself (wake_me)" : "Your timer"}>
              <span className="place-meta grow">
                ⏲ {t.dailyAt ? `every day at ${t.dailyAt}, next` : t.everyMinutes ? `every ${period(t.everyMinutes)}, next` : "once,"} {new Date(t.atMs).toLocaleString()}
                {t.by === "chat" ? " · set by the chat" : `: ${t.note}`}
              </span>
              <button className="icon-btn danger" title="Remove the timer" onClick={() => void run(() => desktop.removeTimer(t.id))}>
                ×
              </button>
            </div>
          ))}
          {timerFor === grant.chat && (
            <div className="add-server">
              <input value={timerNote} placeholder="What to do (check CI…)" onChange={(e) => setTimerNote(e.target.value)} />
              <select value={timerWhen} onChange={(e) => setTimerWhen(e.target.value)}>
                <option value="morning">every morning at 9</option>
                <option value="hourly">every hour</option>
                <option value="once">once, in an hour</option>
              </select>
              <button className="btn primary" disabled={!timerNote.trim()} onClick={() => void addTimer(grant)}>
                Add timer
              </button>
            </div>
          )}
        </div>
      ))}
    </>
  );
}
