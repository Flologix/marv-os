import { type ReactNode, useEffect, useRef, useState } from "react";
import {
  type AppInfo,
  type BudgetView,
  type ChatSettings,
  type HangReports,
  type HarnessInfo,
  type MarvConfig,
  type OwnAgent,
  type TerminalAgent,
  type Model,
  type UsageView,
  desktop,
} from "../lib/backend";
import { reportsText, tokens } from "../lib/format";
import { HARNESS_INSTALL, PERMISSION_MODES, permissionModes } from "../lib/harness";
import { type PlanStatus, aboutPlan } from "../lib/plan";
import { LINUX } from "../lib/platform";
import { type ThemePref, applyTheme, storedTheme } from "../lib/theme";
import { MarvPlanCard, usePlanStatus } from "./MarvPlan";
import { ModelPicker } from "./ModelPicker";
import { Martian } from "./Martian";

const money = (usd: number) => `$${usd.toFixed(2)}`;

/** This computer as the web view sees it: "Linux (x86_64)". */
function computerName(): string {
  const ua = navigator.userAgent;
  const os = /Mac OS X/.test(ua) ? "macOS" : /Windows/.test(ua) ? "Windows" : /Linux/.test(ua) ? "Linux" : "this computer";
  const arch = /x86_64|x64|amd64/i.test(ua) ? "x86_64" : /aarch64|arm64/i.test(ua) ? "ARM64" : "";
  return arch ? `${os} (${arch})` : os;
}
const KINDS: { kind: "folder" | "chat" | "core" | "grant"; title: string }[] = [
  { kind: "folder", title: "folders" },
  { kind: "chat", title: "chats" },
  { kind: "core", title: "models" },
  { kind: "grant", title: "background" },
];

/** The page's sections, in order: the menu and the page both follow this list, so they never disagree. */
const SECTIONS = [
  { id: "plan", title: "Account", purpose: "Your Marv account and plan." },
  { id: "harnesses", title: "Agents", purpose: "The coding agents your chats run, on your own logins." },
  { id: "model", title: "Model", purpose: "Which model Marv's own chats use." },
  { id: "spending", title: "Spending", purpose: "What chats spent, and daily limits." },
  { id: "appearance", title: "Appearance", purpose: "" },
  { id: "about", title: "About", purpose: "" },
] as const;
type SectionId = (typeof SECTIONS)[number]["id"];

/** Where a model id is sent: its prefix routes it (a plan's own models, Ollama), anything else goes to OpenRouter. */
type Route = "openrouter" | "ollama" | "kimi" | "chatgpt" | "zai";
function routeOf(model: string): Route {
  const prefix = model.split("/", 1)[0];
  return prefix === "ollama" || prefix === "kimi" || prefix === "chatgpt" || prefix === "zai" ? prefix : "openrouter";
}

/**
 * Kernel #53, Settings (the gear), in sections a person can scan: the Marv plan, how Marv reaches a model (the
 * connections, the default and small models), spending (budgets and usage), the theme, and about (version, plan,
 * hang reports, onboarding again). Each change is the person's click, saved by the desktop's own commands.
 */
export function SettingsPage({
  info,
  onInfo,
  settings,
  onSettings,
  models,
  onReloadModels,
  usage,
  onUsage,
  folder,
  folderName,
  onSetup,
  onApps,
  onOpenChat,
}: {
  info: AppInfo;
  onInfo: (info: AppInfo) => void;
  settings: ChatSettings;
  onSettings: (s: ChatSettings) => void;
  models: Model[];
  onReloadModels: () => void;
  usage: UsageView | null;
  onUsage: (u: UsageView) => void;
  /** The folder selected in Folders: "For <folder>" budgets. */
  folder: string;
  folderName: string;
  /** "plan": the provider step again; "first": the onboarding again. */
  onSetup: (which: "plan" | "first") => void;
  /** Apps, devices and permissions (their own page). */
  onApps: () => void;
  /** Goes to a chat (the problem report's, once it's made). */
  onOpenChat: (id: string) => void;
}) {
  const plan = usePlanStatus();
  const [signingIn, setSigningIn] = useState(false);
  const [config, setConfig] = useState<MarvConfig | null>(null);
  const [small, setSmall] = useState("");
  const [budgetUsd, setBudgetUsd] = useState("");
  const [theme, setTheme] = useState<ThemePref>(storedTheme);
  const [message, setMessage] = useState<{ at: SectionId; text: string; error: boolean } | null>(null);
  const [reports, setReports] = useState<HangReports | null>(null);
  const [version, setVersion] = useState<string | null>(null);
  const [kernel, setKernel] = useState<string | null>(null);
  const [updateCheck, setUpdateCheck] = useState(true);
  const [agents, setAgents] = useState<TerminalAgent[]>([]);
  const [picker, setPicker] = useState<{ tab?: "openrouter" | "plan" | "ollama"; n: number } | null>(null);
  const [active, setActive] = useState<SectionId>("plan");
  const page = useRef<HTMLElement>(null);

  useEffect(() => {
    desktop.hangReports().then(setReports, () => setReports(null));
    desktop.appVersion().then(setVersion, () => setVersion(null));
    desktop.updateCheckOn().then(setUpdateCheck, () => {});
    desktop.about().then((a) => setKernel(a.version), () => setKernel(null));
    desktop.terminalAgents().then(setAgents, () => setAgents([]));
  }, []);

  useEffect(() => {
    desktop.marvConfig().then(
      (c) => (setConfig(c), setSmall(c.smallModel ?? "")),
      (e) => setMessage({ at: "model", text: String(e), error: true }),
    );
  }, []);

  // The section nav follows the scroll: the last section whose heading has passed the top third.
  useEffect(() => {
    const el = page.current;
    if (!el) return;
    const onScroll = () => {
      const top = el.getBoundingClientRect().top + el.clientHeight / 3;
      let current: SectionId = SECTIONS[0].id;
      for (const s of SECTIONS) {
        const section = document.getElementById(`settings-${s.id}`);
        if (section && section.getBoundingClientRect().top <= top) current = s.id;
      }
      if (el.scrollTop + el.clientHeight >= el.scrollHeight - 4) current = SECTIONS[SECTIONS.length - 1].id;
      setActive(current);
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  const go = (id: SectionId) => {
    document.getElementById(`settings-${id}`)?.scrollIntoView({ behavior: "smooth", block: "start" });
    setActive(id);
  };

  const reporting = useRef(false);
  async function guard(at: SectionId, action: () => Promise<void>, done?: string) {
    setMessage(null);
    try {
      await action();
      if (done) setMessage({ at, text: done, error: false });
    } catch (e) {
      setMessage({ at, text: String(e), error: true });
    }
  }

  const said = (at: SectionId) =>
    message?.at === at ? <p className={`settings-message${message.error ? " error" : ""}`}>{message.text}</p> : null;

  const saveSmall = () =>
    guard(
      "model",
      async () => {
        const next = await desktop.setMarvConfig({ smallModel: small.trim() });
        setConfig(next);
        setSmall(next.smallModel ?? "");
      },
      "Small model saved: chats use it from their next start.",
    );

  const setBudget = (scope: "global" | "folder", usd: number | null) =>
    guard("spending", async () => {
      onUsage(await desktop.setBudget(scope, scope === "folder" ? folder : null, usd));
      setBudgetUsd("");
    });

  const removeBudget = (b: BudgetView) => guard("spending", async () => onUsage(await desktop.setBudget(b.scope, b.target || null, null)));

  const pick = (model: string) => model !== settings.model && onSettings({ ...settings, model });
  const status = plan.status;

  const body: Record<SectionId, ReactNode> = {
    plan: (
      <>
        <MarvPlanCard
          status={status}
          checked={plan.checked}
          loadError={plan.error}
          onStatus={plan.setStatus}
          signingIn={signingIn}
          onSigningIn={setSigningIn}
          onInfo={onInfo}
          onReloadModels={onReloadModels}
        />
      </>
    ),
    harnesses: (
      <>
        <Harnesses />
      </>
    ),
    model: (
      <>
        <h3 className="settings-sub">Connections</h3>
        <Connections
          info={info}
          onInfo={onInfo}
          plan={status}
          model={settings.model}
          models={models}
          onReloadModels={onReloadModels}
          onSetup={onSetup}
          onSignIn={() => (setSigningIn(true), go("plan"))}
          onPick={pick}
          onChoose={(tab) => setPicker((p) => ({ tab, n: (p?.n ?? 0) + 1 }))}
          guard={(action, done) => guard("model", action, done)}
        />
        {!info.ready && (
          <p className="settings-message warn">
            The provider Marv is set to needs a key or a sign-in.{" "}
            <button className="link" onClick={() => onSetup("plan")}>
              Set it up…
            </button>
          </p>
        )}

        <h3 className="settings-sub">Default model</h3>
        <p className="settings-hint">New chats start with it; switch a chat's own from its top bar.</p>
        {config?.model && config.model !== settings.model && (
          <p className="settings-hint">
            <code>marv</code> in a terminal uses <code>{config.model}</code> (from ~/.marv/config.json).{" "}
            <button className="link" onClick={() => pick(config.model!)}>
              Use it here too
            </button>
          </p>
        )}
        <ModelSelect
          key={picker?.n ?? 0}
          model={settings.model}
          models={models}
          info={info}
          plan={status}
          open={picker !== null}
          tab={picker?.tab}
          onOpen={(open) => setPicker(open ? { n: picker?.n ?? 0 } : null)}
          onSetupPlan={info.offerZai || info.offerKimi || info.offerChatgpt ? () => onSetup("plan") : undefined}
          onPick={(model) => (pick(model), setPicker(null))}
        />

        <details className="settings-more">
          <summary>More</summary>
          <h3 className="settings-sub">Small model</h3>
          <p className="settings-hint">Marv's side jobs (suggestions) and agents that ask for a small one. Empty: the chat's own model.</p>
          <div className="settings-inline">
            <input
              className="mono grow"
              list="settings-models"
              placeholder="the chat's model"
              value={small}
              spellCheck={false}
              onChange={(e) => setSmall(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void saveSmall()}
              aria-label="Small model"
            />
            <datalist id="settings-models">
              {models.slice(0, 200).map((m) => (
                <option key={m.id} value={m.id} />
              ))}
            </datalist>
            <button className="btn" disabled={!config || small.trim() === (config.smallModel ?? "")} onClick={() => void saveSmall()}>
              Save
            </button>
          </div>
        </details>
        {config?.problem && <p className="settings-message error">Your Marv settings file isn't used: {config.problem}</p>}
        {said("model")}
      </>
    ),
    spending: (
      <>
        {usage ? (
          <>
            <div className="spend-stats">
              <Stat label="Today" value={money(usage.todayUsd)} sub={`${usage.todayRequests} requests`} />
              <Stat label="This week" value={money(usage.weekUsd)} />
            </div>
            <h3 className="settings-sub">Daily limits</h3>
            <p className="settings-hint">
              At its limit a chat asks before going on, and one working while you're away pauses. Unpriced tokens (a plan, a local model) don't
              count.
            </p>
            <div className="settings-list">
              {usage.budgets.length === 0 && <div className="settings-item faint">No limits yet: nothing stops for spend.</div>}
              {usage.budgets.map((b) => (
                <div key={`${b.scope}:${b.target}`} className="settings-item" title={`${b.percent}% of today's budget`}>
                  <span className={`tone-dot ${b.reached ? "waiting" : b.warning ? "paused" : "idle"}`} aria-hidden="true" />
                  <span className="grow">
                    <b>{b.name}</b>{" "}
                    <span className="muted">
                      {money(b.spentUsd)} of {money(b.capUsd)} today{b.reached ? " · reached" : b.warning ? " · over 80%" : ""}
                    </span>
                  </span>
                  <button className="link danger" onClick={() => void removeBudget(b)}>
                    Remove
                  </button>
                </div>
              ))}
              <div className="settings-item add">
                <span className="grow muted">Add a limit</span>
                <input type="number" min="0.01" step="0.5" placeholder="$ a day" value={budgetUsd} onChange={(e) => setBudgetUsd(e.target.value)} aria-label="Dollars a day" />
                <button className="btn" disabled={!(Number(budgetUsd) > 0)} onClick={() => void setBudget("folder", Number(budgetUsd))} title={`Only chats in ${folderName}`}>
                  For {folderName}
                </button>
                <button className="btn" disabled={!(Number(budgetUsd) > 0)} onClick={() => void setBudget("global", Number(budgetUsd))}>
                  For everything
                </button>
              </div>
            </div>
            {usage.rows.length > 0 && (
              <details className="settings-more">
                <summary>Where it went</summary>
                {KINDS.map(({ kind, title }) => {
                  const rows = usage.rows.filter((r) => r.kind === kind);
                  if (rows.length === 0) return null;
                  return (
                    <div key={kind} className="usage-kind">
                      <h3 className="settings-sub">By {title}</h3>
                      {rows.map((r) => (
                        <div key={r.key} className="place-row" title={r.key}>
                          <span className="grow">{r.name}</span>
                          <span className="muted mono small">
                            {money(r.todayUsd)} today · {money(r.weekUsd)} week
                            {r.weekTokens > 0 ? ` · ${Math.round(r.weekTokens / 1000)}k unpriced tokens` : ""}
                          </span>
                        </div>
                      ))}
                    </div>
                  );
                })}
              </details>
            )}
            {usage.problem && <p className="settings-message error">Your budgets file isn't used (every budget counts as reached): {usage.problem}</p>}
          </>
        ) : (
          <p className="faint small">Loading today's spend…</p>
        )}
        {said("spending")}
      </>
    ),
    appearance: (
      <>
        <div className="settings-list">
          <div className="settings-item">
            <span className="grow">
              <b>Theme</b> <span className="muted">Flexoki light or dark</span>
            </span>
            <span className="segmented" role="radiogroup" aria-label="Theme">
              {(["system", "light", "dark"] as const).map((t) => (
                <button key={t} role="radio" aria-checked={theme === t} className={theme === t ? "on" : ""} onClick={() => (setTheme(t), applyTheme(t))}>
                  {t}
                </button>
              ))}
            </span>
          </div>
        </div>
      </>
    ),
    about: (
      <>
        <div className="about-card">
          <Martian size={56} className="toon" />
          <div className="about-name">Marv OS</div>
          <div className="muted mono small">Version {version ?? "…"}</div>
          <dl className="about-facts">
            <dt>Plan</dt>
            <dd>{aboutPlan(status)}</dd>
            <dt>Marv</dt>
            <dd className="mono">{kernel ?? "…"}</dd>
            <dt>Agents</dt>
            <dd>{agents.filter((a) => a.installed).map((a) => a.name).join(", ") || "…"}</dd>
            <dt>Computer</dt>
            <dd>{computerName()}</dd>
          </dl>
          <div className="switch">
            <button
              type="button"
              role="switch"
              aria-checked={updateCheck}
              aria-labelledby="update-check"
              className={`toggle${updateCheck ? " on" : ""}`}
              onClick={() => guard("about", () => desktop.setUpdateCheck(!updateCheck).then(() => setUpdateCheck(!updateCheck)))}
            />
            <div>
              <div id="update-check">Tell me when a new version is out</div>
              <div className="muted small">Once a day Marv OS asks GitHub for the latest release. Nothing else is sent.</div>
            </div>
          </div>
          <div className="about-actions">
            <button className="btn" onClick={() => onSetup("first")}>
              Run the setup again
            </button>
            <button
              className="btn"
              title="Your own coding agent reads Marv OS's log and what's known about this computer (never your conversations or keys), tells you what it thinks went wrong, and helps you write the report"
              onClick={() =>
                guard("about", async () => {
                  // One triage chat per click: a second click while the first starts would open another.
                  if (reporting.current) return;
                  reporting.current = true;
                  try {
                    const pick = ["claude-code", "codex", "opencode"].find((id) => agents.some((a) => a.id === id && a.installed)) ?? null;
                    const [chat, first] = await desktop.startTriage(pick);
                    onOpenChat(chat.id);
                    await desktop.chatTerminalType(chat.id, first);
                  } finally {
                    reporting.current = false;
                  }
                })
              }
            >
              Report a problem
            </button>
            <button
              className="btn"
              title="What went wrong or unexpectedly, without your conversations or keys: paste it into a problem report"
              onClick={() =>
                guard(
                  "about",
                  async () => {
                    const log = await desktop.logsRecent();
                    if (!log) throw new Error("The log is empty.");
                    await desktop.clipboardWrite(log);
                  },
                  "Copied the log's recent lines.",
                )
              }
            >
              Copy the log
            </button>
            <button className="btn" onClick={() => guard("about", () => desktop.logsOpen())}>
              Open the logs folder
            </button>
            {(reports?.count ?? 0) > 0 && (
              <button className="btn" title={reports?.dir} onClick={() => guard("about", () => desktop.openHangReports())}>
                {reportsText(reports?.count ?? 0)}
              </button>
            )}
          </div>
        </div>
        {said("about")}
      </>
    ),
  };

  return (
    <main className="place settings-page" aria-label="Settings" ref={page}>
      <div className="settings-layout">
        <nav className="settings-nav" aria-label="Settings sections">
          {SECTIONS.map((s) => (
            <button key={s.id} className={active === s.id ? "on" : ""} aria-current={active === s.id ? "true" : undefined} onClick={() => go(s.id)}>
              {s.title}
            </button>
          ))}
          <button onClick={onApps} title="What chats can reach: apps, devices and permissions">
            Apps & permissions ›
          </button>
        </nav>
        <div className="settings-body">
          <h1 className="place-title">Settings</h1>
          {SECTIONS.map((s) => (
            <Section key={s.id} id={s.id}>
              {body[s.id]}
            </Section>
          ))}
        </div>
      </div>
    </main>
  );
}

const LOGIN: Record<HarnessInfo["login"], string> = {
  signed_in: "signed in",
  api_key: "uses ANTHROPIC_API_KEY",
  signed_out: "not signed in: run claude in a terminal, then /login",
  unknown: "sign-in not checked here",
};

/** The harness plan, phase 2: each harness found, its version and login, and its chats now. */
function Harnesses() {
  const [list, setList] = useState<HarnessInfo[] | null>(null);
  const [mode, setMode] = useState("own");
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    desktop.harnesses().then((l) => alive && setList(l), () => alive && setList([]));
    desktop.defaultPermissions().then((m) => alive && setMode(m), () => {});
    return () => {
      alive = false;
    };
  }, []);
  if (!list) return <p className="faint small">Looking for agents…</p>;
  return (
    <>
      <div className="settings-list">
        {list.map((h) => (
          <div key={h.name} className="settings-item">
            <span className="grow">
              <b>{h.name}</b>{" "}
              <span className="muted">
                {h.path
                  ? `${h.login === "unknown" ? "uses its own login" : LOGIN[h.login]} · ${h.loaded} chat${h.loaded === 1 ? "" : "s"} loaded, ${h.asleep} asleep`
                  : `not installed: ${HARNESS_INSTALL[h.id] ?? "install it, then sign in once in a terminal"}`}
              </span>
            </span>
            {h.path && (
              <span className="faint small mono" title={h.path}>
                {h.version ?? "version unknown"}
              </span>
            )}
          </div>
        ))}
        {list.some((h) => h.path) && (
          <div className="settings-item">
            <span className="grow">
              <b>Outer sandbox</b>{" "}
              <span className="muted">
                {!list[0].sandboxAvailable
                  ? "Marv's sandbox can't run here (it needs bubblewrap, on Linux), so these agents reach everything you can"
                  : list[0].sandbox
                    ? "on: each agent sees its chat's folder and its own settings; your other projects, keys and logins are hidden"
                    : "off: these agents reach everything you can, as in a terminal"}
              </span>
            </span>
            {list[0].sandboxAvailable && (
              <button className="btn" onClick={() => (setFailed(null), desktop.setHarnessSandbox(!list[0].sandbox).then(setList, (e) => setFailed(String(e))))}>
                {list[0].sandbox ? "Turn off" : "Turn on"}
              </button>
            )}
          </div>
        )}
        <div className="settings-item">
          <span className="grow">
            <b>Permissions for new chats</b>{" "}
            <span className="muted">{PERMISSION_MODES.find((m) => m.id === mode)?.text}; each chat can change its own from its header</span>
          </span>
          <select
            value={mode}
            aria-label="Permissions for new chats"
            onChange={(e) => {
              const next = e.target.value;
              setFailed(null);
              void desktop.setDefaultPermissions(next).then(() => setMode(next), (e) => setFailed(String(e)));
            }}
          >
            {permissionModes(!!list[0]?.sandboxAvailable || mode === "full").map((m) => (
              <option key={m.id} value={m.id}>
                {m.name}
              </option>
            ))}
          </select>
        </div>
      </div>
      {failed && <p className="notice error">{failed}</p>}
      <OwnAgents />
      <p className="faint small">
        Start one from a new chat, or a plain Terminal for anything else. An idle agent sleeps after 10 minutes and
        wakes where it left off.
      </p>
    </>
  );
}

/**
 * The person's own terminal agents: a name and the command that starts it (any program, with its arguments), and
 * optionally the arguments that continue its latest conversation. Each is in the new chat's picker at once, as its own
 * terminal; the known agents' extras (mesh tools, team briefs, turn reports) aren't set up for it.
 */
function OwnAgents() {
  const [list, setList] = useState<OwnAgent[]>([]);
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [resume, setResume] = useState("");
  const [said, setSaid] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    desktop.ownAgents().then((l) => alive && setList(l), () => {});
    return () => {
      alive = false;
    };
  }, []);
  const add = async () => {
    try {
      setList(await desktop.addOwnAgent(name, command, resume));
      setName("");
      setCommand("");
      setResume("");
      setAdding(false);
      setSaid(null);
    } catch (e) {
      setSaid(String(e));
    }
  };
  return (
    <div className="settings-list">
      {list.map((a) => (
        <div key={a.id} className="settings-item">
          <span className="grow">
            <b>{a.name}</b> <span className="muted mono small">{a.command}</span>
            {a.resume && <span className="faint small"> · continues with {a.resume}</span>}
          </span>
          <button className="btn" onClick={() => desktop.removeOwnAgent(a.id).then(setList, (e) => setSaid(String(e)))}>
            Remove
          </button>
        </div>
      ))}
      <div className="settings-item">
        <span className="grow">
          <b>Your own agent</b>{" "}
          <span className="muted">any terminal agent or program: it starts in a chat's terminal, in the chat's folder</span>
        </span>
        <button className="btn" aria-expanded={adding} onClick={() => (setAdding(!adding), setSaid(null))}>
          {adding ? "Cancel" : "Add agent"}
        </button>
      </div>
      {adding && (
        <div className="conn-form">
          <div className="settings-inline">
            <input className="grow" placeholder="Name, e.g. Hermes" value={name} autoFocus onChange={(e) => setName(e.target.value)} aria-label="Agent name" />
          </div>
          <div className="settings-inline">
            <input
              className="mono grow"
              placeholder="Command, e.g. hermes  or  node ~/tools/agent.js --fast"
              value={command}
              spellCheck={false}
              onChange={(e) => setCommand(e.target.value)}
              aria-label="Command that starts it"
            />
          </div>
          <div className="settings-inline">
            <input
              className="mono grow"
              placeholder="Continue with (optional), e.g. --continue"
              value={resume}
              spellCheck={false}
              onChange={(e) => setResume(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && name.trim() && command.trim() && void add()}
              aria-label="Arguments that continue its latest conversation"
            />
            <button className="btn primary" disabled={!name.trim() || !command.trim()} onClick={() => void add()}>
              Add
            </button>
          </div>
          <span className="help">
            A word with spaces goes in double quotes. Without “continue with”, it starts afresh each time its chat
            wakes. In Home it runs inside Marv's sandbox, which hides your home's settings and keys.
          </span>
        </div>
      )}
      {said && <p className="notice error">{said}</p>}
    </div>
  );
}

function Section({ id, children }: { id: SectionId; children: ReactNode }) {
  const { title, purpose } = SECTIONS.find((s) => s.id === id)!;
  return (
    <section id={`settings-${id}`} className="settings-section" aria-labelledby={`settings-${id}-h`}>
      <header>
        <h2 id={`settings-${id}-h`}>{title}</h2>
        {purpose && <p>{purpose}</p>}
      </header>
      {children}
    </section>
  );
}

function Stat({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="spend-stat">
      <span className="faint small">{label}</span>
      <b className="mono">{value}</b>
      {sub && <span className="faint small">{sub}</span>}
    </div>
  );
}

type ConnState = "use" | "ready" | "none";
const PILL: Record<ConnState, string> = { use: "In use", ready: "Ready", none: "Not set up" };

interface Conn {
  id: string;
  title: string;
  detail: string;
  state: ConnState;
  actions: ReactNode;
  /** Shown under the row when it's expanded (a key field, how to set it up). */
  form?: ReactNode;
}

/** Every way to reach a model, one row each: the Marv plan, OpenRouter, a plan you already pay for, Ollama. The one
 * the default model goes through is "In use". */
function Connections({
  info,
  onInfo,
  plan,
  model,
  models,
  onReloadModels,
  onSetup,
  onSignIn,
  onPick,
  onChoose,
  guard,
}: {
  info: AppInfo;
  onInfo: (info: AppInfo) => void;
  plan: PlanStatus | null;
  model: string;
  models: Model[];
  onReloadModels: () => void;
  onSetup: (which: "plan" | "first") => void;
  onSignIn: () => void;
  onPick: (model: string) => void;
  /** Open the default model's picker on a tab (after "Use this" where there's more than one model to choose from). */
  onChoose: (tab: "openrouter" | "plan" | "ollama") => void;
  guard: (action: () => Promise<void>, done?: string) => Promise<void>;
}) {
  const [open, setOpen] = useState<string | null>(null);
  const [keys, setKeys] = useState<Record<string, string>>({});
  const route = routeOf(model);
  const planKey = !!plan?.signedIn && plan.keyInUse;
  const ownKey = info.hasKey && !planKey;
  const local = models.filter((m) => m.local);
  const state = (inUse: boolean, ready: boolean): ConnState => (inUse && ready ? "use" : ready ? "ready" : "none");
  const toggle = (id: string) => setOpen((o) => (o === id ? null : id));

  /** Switch to a provider: its chosen-provider setting, and a model it serves. */
  const switchTo = (provider: "openrouter" | "ollama" | "zai" | "kimi" | "chatgpt", first?: string) =>
    guard(async () => {
      onInfo(await desktop.setProvider(provider));
      if (first) onPick(first);
      else onChoose(provider === "ollama" ? "ollama" : "openrouter");
    });

  const firstOf = (prefix: string) => models.find((m) => m.id.startsWith(`${prefix}/`))?.id;

  const keyForm = (id: string, placeholder: string, from: { label: string; url: string }, save: (key: string) => Promise<AppInfo>) => (
    <div className="conn-form">
      <div className="settings-inline">
        <input
          className="mono grow"
          type="password"
          placeholder={placeholder}
          value={keys[id] ?? ""}
          spellCheck={false}
          autoFocus
          onChange={(e) => setKeys({ ...keys, [id]: e.target.value })}
          onKeyDown={(e) => e.key === "Enter" && (keys[id] ?? "").trim() && void saveKey(id, save)}
          aria-label={placeholder}
        />
        <button className="btn primary" disabled={!(keys[id] ?? "").trim()} onClick={() => void saveKey(id, save)}>
          Save key
        </button>
      </div>
      <span className="help">
        From{" "}
        <button className="link" onClick={() => void desktop.openUrl(from.url)}>
          {from.label} ↗
        </button>
        . Stays on this computer.
      </span>
    </div>
  );

  const saveKey = (id: string, save: (key: string) => Promise<AppInfo>) =>
    guard(async () => {
      onInfo(await save(keys[id] ?? ""));
      setKeys({ ...keys, [id]: "" });
      setOpen(null);
      onReloadModels();
    }, "Key saved.");

  const keyButton = (id: string, has: boolean) => (
    <button className="btn" aria-expanded={open === id} onClick={() => toggle(id)}>
      {open === id ? "Cancel" : has ? "Replace key" : "Add key"}
    </button>
  );

  const thisButton = (onClick: () => void, title?: string) => (
    <button className="btn" onClick={onClick} title={title}>
      Use this
    </button>
  );

  const rows: Conn[] = [];

  // The Marv plan: its key lives where an OpenRouter key does. Signed out, Plan & account above offers it (twice
  // was one too many).
  if (plan?.signedIn) {
    const offered = !!plan?.signedIn && plan.hasModels && plan.online && !plan.keyInUse && plan.status !== "canceled";
    const s = state(route === "openrouter", planKey);
    rows.push({
      id: "marv",
      title: "Marv plan",
      detail: !plan?.signedIn
        ? "A monthly model allowance, no key to set up"
        : !plan.hasModels
          ? "Your plan doesn't include models"
          : planKey
            ? "The plan's allowance pays for OpenRouter models"
            : "Signed in; the plan's key isn't the one in use",
      state: s,
      actions: !plan?.signedIn ? (
        <button className="btn" onClick={onSignIn}>
          Sign in
        </button>
      ) : offered ? (
        thisButton(
          () =>
            void guard(async () => {
              onInfo(await desktop.planUseKey());
              const defaultModel = plan.allowance?.defaultModel;
              if (defaultModel) onPick(defaultModel);
              onReloadModels();
            }),
          "Replaces the OpenRouter key on this computer with the plan's",
        )
      ) : s === "ready" ? (
        thisButton(() => void switchTo("openrouter", plan.allowance?.defaultModel ?? undefined))
      ) : null,
    });
  }

  rows.push({
    id: "openrouter",
    title: "OpenRouter key",
    detail: ownKey ? "Your own key · hundreds of models, pay per use" : planKey ? "Saving your own key replaces the plan's" : "Hundreds of models, pay per use",
    state: state(route === "openrouter", ownKey),
    actions: (
      <>
        {ownKey && route !== "openrouter" && thisButton(() => void switchTo("openrouter"))}
        {keyButton("openrouter", ownKey)}
      </>
    ),
    form: keyForm("openrouter", "sk-or-…", { label: "openrouter.ai/settings/keys", url: "https://openrouter.ai/settings/keys" }, desktop.setApiKey),
  });

  if (info.offerChatgpt) {
    const signed = !!info.chatgptAccount;
    rows.push({
      id: "chatgpt",
      title: "ChatGPT",
      detail: signed ? `Signed in as ${info.chatgptAccount}` : "Your ChatGPT Plus or Pro plan, within its usage limits",
      state: state(route === "chatgpt", signed),
      actions: signed ? (
        <>
          {route !== "chatgpt" && thisButton(() => void switchTo("chatgpt", firstOf("chatgpt")))}
          <button className="btn" onClick={() => void guard(async () => (onInfo(await desktop.chatgptSignOut()), onReloadModels()))}>
            Sign out
          </button>
        </>
      ) : (
        <button className="btn" onClick={() => onSetup("plan")}>
          Sign in…
        </button>
      ),
    });
  }

  if (info.offerKimi) {
    rows.push({
      id: "kimi",
      title: "Kimi Code",
      detail: info.hasKimiKey ? "Key saved · the plan's usage limits apply" : "Kimi's coding models on the plan you subscribe to",
      state: state(route === "kimi", info.hasKimiKey),
      actions: (
        <>
          {info.hasKimiKey && route !== "kimi" && thisButton(() => void switchTo("kimi", firstOf("kimi") ?? "kimi/kimi-for-coding"))}
          {keyButton("kimi", info.hasKimiKey)}
        </>
      ),
      form: keyForm("kimi", "Kimi Code API key", { label: "kimi.com/code/console", url: "https://www.kimi.com/code/console" }, desktop.setKimiKey),
    });
  }

  if (info.offerZai) {
    rows.push({
      id: "zai",
      title: "Z.ai GLM Coding Plan",
      detail: info.hasZaiKey ? "Key saved · the plan's usage limits apply" : "GLM on the plan you subscribe to",
      state: state(route === "zai", info.hasZaiKey),
      actions: (
        <>
          {info.hasZaiKey && route !== "zai" && thisButton(() => void switchTo("zai", firstOf("zai") ?? "zai/glm-5.3"))}
          {keyButton("zai", info.hasZaiKey)}
        </>
      ),
      form: keyForm("zai", "Coding Plan API key", { label: "z.ai/manage-apikey/apikey-list", url: "https://z.ai/manage-apikey/apikey-list" }, desktop.setZaiKey),
    });
  }

  rows.push({
    id: "ollama",
    title: "Ollama",
    detail: local.length ? `On this computer · ${local.length} model${local.length === 1 ? "" : "s"}, free and private` : "On this computer: free and private, needs a good GPU",
    state: state(route === "ollama", local.length > 0),
    actions: local.length ? (
      route !== "ollama" ? thisButton(() => void switchTo("ollama")) : null
    ) : (
      <button className="btn" aria-expanded={open === "ollama"} onClick={() => toggle("ollama")}>
        {open === "ollama" ? "Close" : "How to set up"}
      </button>
    ),
    form: (
      <div className="conn-form">
        {LINUX ? (
          <>
            <span className="help">Install Ollama, then start it:</span>
            <pre className="code">sudo systemctl enable --now ollama</pre>
          </>
        ) : (
          <span className="help">Install Ollama from ollama.com, then start it.</span>
        )}
        <div className="settings-inline">
          <button className="btn" onClick={onReloadModels}>
            Check again
          </button>
          <button className="btn" onClick={() => onChoose("ollama")}>
            Download a model…
          </button>
        </div>
      </div>
    ),
  });

  return (
    <div className="settings-list conns">
      {rows.map((r) => (
        <div key={r.id} className={`conn ${r.state}${open === r.id ? " open" : ""}`}>
          <div className="settings-item">
            <span className="grow conn-text">
              <span className="conn-title">
                <b>{r.title}</b>
                <span className={`pill ${r.state === "use" ? "ok" : r.state === "ready" ? "ready" : "off"}`}>{PILL[r.state]}</span>
              </span>
              <span className="muted small">{r.detail}</span>
            </span>
            <span className="conn-actions">{r.actions}</span>
          </div>
          {open === r.id && r.form}
        </div>
      ))}
      <div className="settings-item conn-foot">
        <span className="grow faint small">Not sure which? The setup compares them side by side.</span>
        <button className="link" onClick={() => onSetup("plan")}>
          Open the provider setup…
        </button>
      </div>
    </div>
  );
}

/** The default model, collapsed to its value; a click opens the searchable picker (bounded, never the whole catalogue
 * on the page), with the Marv plan's default model first. */
function ModelSelect({
  model,
  models,
  info,
  plan,
  open,
  tab,
  onOpen,
  onSetupPlan,
  onPick,
}: {
  model: string;
  models: Model[];
  info: AppInfo;
  plan: PlanStatus | null;
  open: boolean;
  tab?: "openrouter" | "plan" | "ollama";
  onOpen: (open: boolean) => void;
  onSetupPlan?: () => void;
  onPick: (model: string) => void;
}) {
  const current = models.find((m) => m.id === model);
  const meta = !current
    ? null
    : current.local
      ? "Ollama · on this computer"
      : current.plan
        ? `${current.plan}${current.context ? ` · ${tokens(current.context)}` : ""}`
        : `OpenRouter · ${tokens(current.context)} · $${current.input.toFixed(2)}/$${current.output.toFixed(2)} per M`;
  const box = useRef<HTMLDivElement>(null);
  // Opened from a connection's "Use this": bring it into view to pick from.
  useEffect(() => {
    if (open && tab) box.current?.scrollIntoView({ behavior: "smooth", block: "center" });
  }, [open, tab]);
  const planDefault = plan?.signedIn && plan.hasModels ? plan.allowance?.defaultModel : null;
  const recommended = planDefault ? [{ id: planDefault, note: "Marv plan default" }] : [];
  return (
    <div ref={box} className={`model-select${open ? " open" : ""}`} onKeyDown={(e) => e.key === "Escape" && open && onOpen(false)}>
      <button className="model-current" aria-expanded={open} onClick={() => onOpen(!open)}>
        <span className="grow model-current-text">
          <span className="mono">
            {current?.local || current?.plan ? current.name : model}
            {planDefault === model && <span className="model-tag">Marv plan default</span>}
          </span>
          {meta && <span className="faint small">{meta}</span>}
        </span>
        <span className="model-current-change">{open ? "Close" : "Change"}</span>
      </button>
      {open && (
        <div className="model-drop">
          <ModelPicker
            model={model}
            remote={models.filter((m) => !m.local)}
            hasKey={info.hasKey}
            onSetupPlan={onSetupPlan}
            onPick={onPick}
            recommended={recommended}
            initialTab={tab}
            autoFocus
          />
        </div>
      )}
    </div>
  );
}
