import { ask } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useState } from "react";
import { type ChatSettings, type Model, type RoleEntry, type TeamEntry, type TeamStarted, desktop } from "../lib/backend";
import {
  type MemberDraft,
  type TemplateShape,
  addMember,
  boundsSummary,
  changeRole,
  draftProblem,
  membersFor,
  missingWrites,
  roleDoes,
  roleLabel,
  scratchTemplate,
  specOf,
  takesWrites,
  writesHint,
} from "../lib/teams";
import { ModelPicker } from "./ModelPicker";

/** The template a dialog opens on: a team's name, or "scratch" (a lead and a builder). */
export type TeamPick = string;

/**
 * Team setup in two steps. First, what kind of team (the built-ins, the folder's and your team files, or from
 * scratch), in plain words. Then who's on it (each member a card: name, role, model) and, optionally, the first task,
 * sent to the lead as your message once the team runs. The rule, wake, worktrees, write paths and "Save as team file"
 * are all still here, under Advanced. Start team: the folder rule, wake, each chat with its role (lead first), with each
 * step's result shown.
 */
export function NewTeam({
  folder,
  folderName,
  settings,
  models,
  hasKey,
  initial,
  onClose,
  onStarted,
}: {
  folder: string;
  folderName: string;
  settings: ChatSettings;
  models: Model[];
  hasKey: boolean;
  /** Open on this template's members (skipping the first step). */
  initial?: TeamPick;
  onClose: () => void;
  /** The team runs; `task`, if any, is for its lead. */
  onStarted: (started: TeamStarted, name: string, task: string) => void;
}) {
  const [teams, setTeams] = useState<TeamEntry[]>([]);
  const [problems, setProblems] = useState<string[]>([]);
  const [hasFile, setHasFile] = useState(false);
  const [startedHere, setStartedHere] = useState<string[]>([]);
  const [meshWake, setMeshWake] = useState<"on" | "off" | "rules">("rules");
  const [roleFiles, setRoleFiles] = useState<string[]>([]);
  const [roles, setRoles] = useState<RoleEntry[]>([]);
  const [step, setStep] = useState<"kind" | "who">(initial ? "who" : "kind");
  const [template, setTemplate] = useState<string>("");
  const [name, setName] = useState("");
  const [drafts, setDrafts] = useState<MemberDraft[]>([]);
  // Phase 3: a member may be one of your own installed agents (Claude Code, Codex, OpenCode, …): the new chat's list
  // (`terminal_agents`), Marv aside (the default). Not `harnesses()`, the older list without Codex.
  const [agents, setAgents] = useState<{ id: string; name: string }[]>([]);
  useEffect(() => {
    let alive = true;
    desktop.terminalAgents().then((list) => alive && setAgents(list.filter((a) => a.installed && a.id !== "marv" && a.kind !== "shell")), () => alive && setAgents([]));
    return () => {
      alive = false;
    };
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const claudeCode = agents.length > 0;
  const [task, setTask] = useState("");
  const [worktrees, setWorktrees] = useState(false);
  const [readChats, setReadChats] = useState(true);
  const [keepWorking, setKeepWorking] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const [picking, setPicking] = useState<number | null>(null);
  const [working, setWorking] = useState(false);
  const [result, setResult] = useState<TeamStarted | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    Promise.all([desktop.listTeams(folder), desktop.listRoles(folder)]).then(
      ([list, [roleList, roleProblems]]) => {
        if (!alive) return;
        setTeams(list.teams);
        setProblems([...list.problems, ...roleProblems]);
        setHasFile(list.hasFile);
        setStartedHere(list.here.map((t) => t.name));
        setMeshWake(list.meshWake);
        setRoleFiles(list.roleFiles);
        setRoles(roleList);
        const wanted = initial === "scratch" ? null : list.teams.find((t) => t.name === initial);
        if (initial === "scratch") pick(scratchTemplate(roleList), roleList);
        else if (wanted) pick(wanted, roleList);
        else setStep("kind");
      },
      (e) => alive && setError(String(e)),
    );
    return () => {
      alive = false;
    };
  }, [folder]); // eslint-disable-line react-hooks/exhaustive-deps

  function pick(team: TemplateShape & { name: string }, roleList = roles) {
    setTemplate(team.name);
    setName(team.name);
    setDrafts(membersFor(team, folderName, roleList));
    setWorktrees(team.worktrees);
    setReadChats(team.readChats);
    setResult(null);
    setError(null);
    setStep("who");
  }

  const chosen = teams.find((t) => t.name === template);
  const problem = draftProblem(name, drafts);
  const spec = useMemo(
    () => specOf(name, drafts, { worktrees, readChats, keepWorking, description: chosen?.description ?? null }, settings),
    [name, drafts, worktrees, readChats, keepWorking, chosen, settings],
  );
  const unbounded = missingWrites(drafts, roles);

  async function start() {
    setError(null);
    setWorking(true);
    try {
      const started = await desktop.startTeam(folder, spec);
      setResult(started);
      if (started.ok) onStarted(started, spec.name, task.trim());
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }

  async function save() {
    setError(null);
    setNotice(null);
    const missing = drafts.map((d) => d.role).filter((r, i, all) => all.indexOf(r) === i && !roleFiles.includes(r));
    const roleLine = missing.length ? ` and ${missing.map((r) => `.marv/roles/${r}.md`).join(", ")} with the write paths shown` : "";
    const kept = roleFiles.filter((r) => drafts.some((d) => d.role === r));
    const keptLine = kept.length ? ` Left as they are: ${kept.map((r) => `.marv/roles/${r}.md`).join(", ")}.` : "";
    const brief = chosen && chosen.source === "personal" && chosen.briefText ? `\n\nIts brief from ~/.marv/teams goes into the project's file, for every member's prompt:\n\n${chosen.briefText}` : "";
    if (!(await ask(`Write .marv/team.md in ${folderName}${roleLine}?${keptLine}${brief}`, { title: "Marv", kind: "info" }))) return;
    try {
      let saved = await desktop.saveTeamFile(folder, spec);
      if (saved.exists) {
        if (!(await ask(`${folderName} already has a .marv/team.md. Replace it with this team?`, { title: "Marv", kind: "warning" }))) return;
        saved = await desktop.saveTeamFile(folder, spec, true);
      }
      const kept = saved.kept.length ? ` Left as they were: ${saved.kept.join(", ")}.` : "";
      const trust = saved.trusted ? "" : " The folder's files aren't trusted (or something else changed), so a chat there asks you to /trust them before it uses the team file and roles.";
      setNotice(`Saved ${saved.written.join(", ")}.${kept}${trust}`);
      setHasFile(true);
    } catch (e) {
      setError(String(e));
    }
  }

  const modelLabel = (m: string) => (m === "" || m === "inherit" ? "default model" : m === "small" ? "small model" : m.split("/").pop()!);
  const update = (i: number, patch: Partial<MemberDraft>) => setDrafts(drafts.map((d, j) => (j === i ? { ...d, ...patch } : d)));
  const lead = drafts.find((d) => d.role === "coordinator") ?? drafts[0];

  return (
    // Closes on a press on the backdrop itself, not a click: a text selection that began in the card and ended outside
    // it ends in a click on the backdrop.
    <div className="modal" role="dialog" aria-label="New team" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal-card team-dialog">
        <div className="modal-head">
          <h2>{step === "kind" ? `Set up a team in ${folderName}` : `Your team in ${folderName}`}</h2>
          <button className="icon-btn" title="Close" onClick={onClose}>
            ×
          </button>
        </div>

        {step === "kind" && (
          <>
            <p className="help">
              A team is a few Marv chats that work on {folderName} together: a lead plans and hands out tasks, the others do them and report back. You
              follow along in Teams and step in whenever you like.
            </p>
            <div className="team-cards">
              {teams.map((t) => (
                <button key={t.name} className="team-card" onClick={() => pick(t)} title={t.description}>
                  <b>{t.title}</b>
                  <span className="small">{t.description}</span>
                  <span className="member-pills">
                    {t.members.map((m, i) => (
                      <span key={i} className="member-pill">
                        {roleLabel(m.role)}
                      </span>
                    ))}
                  </span>
                  {t.source !== "built-in" && <span className="faint small">{t.source === "project" ? "this folder's team" : "your team file"}</span>}
                </button>
              ))}
              <button className="team-card scratch" onClick={() => pick(scratchTemplate(roles))}>
                <b>Start from scratch</b>
                <span className="small">A lead and a builder. Add whoever else you need.</span>
              </button>
            </div>
            {problems.length > 0 && <p className="faint small">Not loaded: {problems.join(" · ")}</p>}
            {error && <p className="notice error">{error}</p>}
          </>
        )}

        {step === "who" && (
          <>
            <label className="field team-task">
              <span>What should they work on? (optional)</span>
              <textarea
                rows={3}
                value={task}
                placeholder="e.g. Add a contact form to the site and make sure it sends email"
                onChange={(e) => setTask(e.target.value)}
                disabled={!!result?.ok}
              />
              <span className="faint small">{lead ? `${lead.title} gets this as your first message once the team starts. ` : ""}You can also tell them later, in Teams.</span>
            </label>

            <h3 className="place-h">Members</h3>
            <div className="team-members">
              {drafts.map((d, i) => {
                const role = roles.find((r) => r.name === d.role);
                return (
                  <div key={i} className="team-member">
                    <div className="team-member-line">
                      <span className="member-mark" aria-hidden="true">
                        {roleLabel(d.role).slice(0, 1)}
                      </span>
                      <span className="member-text">
                        <input className="member-name" value={d.title} aria-label="Chat name" spellCheck={false} onChange={(e) => update(i, { title: e.target.value })} />
                        <span className="muted small">{role ? roleDoes(role) : ""}</span>
                      </span>
                      <select value={d.role} aria-label="Role" onChange={(e) => setDrafts(changeRole(drafts, i, e.target.value, folderName, roles))}>
                        {roles.map((r) => (
                          <option key={r.name} value={r.name} title={r.description}>
                            {roleLabel(r.name)}
                          </option>
                        ))}
                      </select>
                      {claudeCode && (
                        <select value={d.harness ?? ""} aria-label="Agent" title="What runs this member: Marv, or one of your own agents (its role and the team's brief go to it at start)" onChange={(e) => update(i, { harness: e.target.value })}>
                          <option value="">Marv</option>
                          {agents.map((a) => (
                            <option key={a.id} value={a.id}>
                              {a.name}
                            </option>
                          ))}
                        </select>
                      )}
                      {d.harness ? (
                        <span className="chip" title="It uses your own login, model and settings">
                          {agents.find((a) => a.id === d.harness)?.name ?? d.harness}
                        </span>
                      ) : (
                        <button className="chip" aria-expanded={picking === i} title={`This chat's model${d.model ? "" : ` (${settings.model})`}`} onClick={() => setPicking(picking === i ? null : i)}>
                          {modelLabel(d.model)}
                        </button>
                      )}
                      <button className="icon-btn danger" title="Remove this member" disabled={drafts.length === 1} onClick={() => setDrafts(drafts.filter((_, j) => j !== i))}>
                        ×
                      </button>
                    </div>
                    {advanced && role && takesWrites(role) && (
                      <label className="team-writes" title="Project-relative folders or files this chat may write (no globs): enforced from its start, for its whole life. Save as team file also writes them into .marv/roles/<role>.md.">
                        may only change{" "}
                        <input value={d.writes} placeholder={writesHint(d.role)} spellCheck={false} onChange={(e) => update(i, { writes: e.target.value })} />
                      </label>
                    )}
                    {advanced && role && <span className="team-bounds">{boundsSummary(role, d.writes)}</span>}
                    {picking === i && !d.harness && (
                      <div className="team-model">
                        <div className="add-buttons">
                          <button className="btn" onClick={() => (update(i, { model: "" }), setPicking(null))}>
                            Default model
                          </button>
                          <button className="btn" onClick={() => (update(i, { model: "small" }), setPicking(null))}>
                            Small model (cheaper)
                          </button>
                        </div>
                        <ModelPicker model={d.model && d.model !== "small" && d.model !== "inherit" ? d.model : settings.model} remote={models} hasKey={hasKey} onPick={(id) => (update(i, { model: id }), setPicking(null))} />
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
            <button className="add-link" onClick={() => setDrafts(addMember(drafts, folderName, roles))} disabled={drafts.length >= 8}>
              + Add a member
            </button>

            <label className="team-switch">
              <span>
                <b>Keep working while I'm away</b>
                <span className="muted small">
                  A teammate's message can start work without you: editing files in {folderName} and running sandboxed commands, up to $2 a day each.
                  Anything else still waits for you.
                </span>
              </span>
              <button type="button" role="switch" aria-checked={keepWorking} aria-label="Keep working while I'm away" className={`toggle${keepWorking ? " on" : ""}`} onClick={() => setKeepWorking(!keepWorking)} />
            </label>
            {unbounded.length > 0 && !advanced && (
              <p className="faint small">
                {unbounded.map((d) => d.title).join(", ")} may change any file in {folderName}.{" "}
                <button className="link" onClick={() => setAdvanced(true)}>
                  Limit which files
                </button>
              </p>
            )}

            <button className="link small team-advanced-toggle" aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>
              {advanced ? "▾" : "▸"} Advanced
            </button>
            {advanced && (
              <div className="team-advanced">
                <label className="team-name">
                  Team name <input value={name} spellCheck={false} onChange={(e) => setName(e.target.value)} />
                </label>
                {unbounded.length > 0 && (
                  <p className="help warn">
                    {unbounded.map((d) => d.title).join(", ")} {unbounded.length === 1 ? "has" : "have"} no file limits: each may edit anywhere in {folderName}, without
                    asking, while working on the lead's tasks. Fill in "may only change" above so no two edit the same file.
                  </p>
                )}
                <label className="help" title="One working tree: everyone sees each other's edits live (a dev server too). Keep the members' file limits apart.">
                  <input type="radio" checked={!worktrees} onChange={() => setWorktrees(false)} /> All in one folder (they see each other's edits live)
                </label>
                <label className="help" title="Each chat starts in its own git worktree (a branch of its own), so they never write over each other; you merge the branches.">
                  <input type="radio" checked={worktrees} onChange={() => setWorktrees(true)} /> Each on its own git branch (you merge their work)
                </label>
                <label className="help" title="Under this rule a chat may read another's conversation (what you typed and what its assistant replied, never what a tool read or ran). A chat you mark private refuses.">
                  <input type="checkbox" checked={readChats} onChange={(e) => setReadChats(e.target.checked)} /> They may read each other's chats (never one you mark
                  private)
                </label>
                <p className="help team-rule-note">
                  Starting the team makes a rule: <b>every chat and terminal Marv under {folderName}</b> (not only these members) may message the others without
                  asking, until you end the team. Stop team stops every chat the rule covers.
                </p>
                {startedHere.length > 0 && (
                  <p className="help warn">
                    {startedHere.length === 1 ? "The team" : "The teams"} {startedHere.join(", ")} in {folderName} will end when you start this one: the chats stay, as
                    ordinary chats.
                  </p>
                )}
                {meshWake === "off" && (
                  <p className="help warn">Wake on mail is off: starting the team turns it to "a rule's wires", so a teammate's message can wake a member.</p>
                )}
                <button className="btn" disabled={problem !== null} title={hasFile ? "Replace this folder's .marv/team.md (asks first)" : "Write .marv/team.md and the missing role files (asks first)"} onClick={() => void save()}>
                  Save as this folder's team file
                </button>
              </div>
            )}

            {result && (
              <ol className="team-steps">
                {result.steps.map((s, i) => (
                  <li key={i} className={s.ok ? "ok" : "bad"}>
                    <b>{s.ok ? "✓" : "✗"} {s.what}</b> <span className="place-meta">{s.detail}</span>
                  </li>
                ))}
              </ol>
            )}
            {notice && <p className="notice">{notice}</p>}
            {error && <p className="notice error">{error}</p>}
            {problem && drafts.length > 0 && <p className="faint small">{problem}</p>}
            <div className="add-buttons">
              {result?.ok ? (
                <button className="btn primary" onClick={onClose}>
                  See the team
                </button>
              ) : (
                <button className="btn primary" disabled={working || problem !== null} title={problem ?? "Start each chat with its role, lead first"} onClick={() => void start()}>
                  {working ? "Starting…" : task.trim() ? "Start team and send the task" : "Start team"}
                </button>
              )}
              {!result?.ok && (
                <button className="btn plain" onClick={() => setStep("kind")}>
                  Back
                </button>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
