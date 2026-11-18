import { ask } from "@tauri-apps/plugin-dialog";
import { useEffect, useRef, useState } from "react";
import { type AgentRow, type ChatRecord, type ChatSettings, type LiveChat, type TeamHere, type WriteOverlap, desktop, isDesktop } from "../lib/backend";
import { type Item, useChat } from "../lib/chat";
import { say } from "../lib/dialog";
import { replyAfterLastUser, roster, teamLead } from "../lib/teams";
import { Board } from "./Board";
import { ToneMarv } from "./Martian";

/**
 * A started team (one-step team setup): its roster (role, state, model, files it shares with another chat, its
 * localhost pages, its last reply, approvals waiting), "Tell the team" (your message, in the coordinator's chat),
 * Stop / Resume / End team, and its board.
 */
export function TeamView({
  team,
  folder,
  chats,
  live,
  settings,
  open,
  onOpenChat,
  onEnded,
  firstTask,
  onFirstTaskSent,
}: {
  team: TeamHere;
  folder: string;
  chats: ChatRecord[];
  live: LiveChat[];
  settings: ChatSettings;
  /** The chat open in Folders, if any: "Tell the team" sends through it when it's the lead (one view per chat). */
  open: { id: string | null; send: (text: string) => void; items: Item[] };
  onOpenChat: (id: string) => void;
  onEnded: () => void;
  /** The task typed in New team: sent to the lead, as you, once its chat is listed. */
  firstTask?: string;
  onFirstTaskSent?: () => void;
}) {
  const [agents, setAgents] = useState<AgentRow[]>([]);
  const [overlaps, setOverlaps] = useState<WriteOverlap[]>([]);
  const [replies, setReplies] = useState<{ chat: string; text: string }[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [text, setText] = useState("");
  const ids = team.chats.join(",");
  const chatsRef = useRef(chats);
  chatsRef.current = chats;

  useEffect(() => {
    const look = () => {
      desktop.activity().then((v) => setAgents(v.activity.rows), () => setAgents([]));
      desktop.writeOverlaps().then(setOverlaps, () => setOverlaps([]));
      const lead = teamLead(team.chats, chatsRef.current);
      if (lead) desktop.teamReports(lead, team.chats).then(setReplies, () => setReplies([]));
      else setReplies([]);
    };
    look();
    const timer = setInterval(look, 4000);
    return () => clearInterval(timer);
  }, [ids]); // eslint-disable-line react-hooks/exhaustive-deps

  const cards = roster(team.chats, chats, live, agents, overlaps, replies);
  const lead = teamLead(team.chats, chats);
  const leadChat = chats.find((c) => c.id === lead);
  const viaOpen = lead !== null && lead === open.id;
  // The lead's own model (a team member keeps the one it was started with).
  const leadSettings = { ...settings, model: leadChat?.settings?.model ?? settings.model };
  const [talk, setTalk] = useState<string | null>(null);
  const own = useChat(viaOpen ? null : talk, leadSettings);
  const reply = replyAfterLastUser(viaOpen ? open.items : own.items);
  const rule = team.rules[0];

  // New team's first task, once the lead's chat shows up (the chats list refreshes after the start).
  useEffect(() => {
    if (!firstTask || !lead) return;
    onFirstTaskSent?.();
    send(firstTask);
  }, [firstTask, lead]); // eslint-disable-line react-hooks/exhaustive-deps

  function tell() {
    const line = text.trim();
    if (!line || !lead) return;
    setText("");
    send(line);
  }

  function send(line: string) {
    if (!lead) return;
    // The person's own message in the coordinator's chat (not a mesh message): typed into its agent's terminal on the
    // desktop, where every chat is its agent's terminal.
    if (isDesktop) {
      void desktop.chatTerminalType(lead, line).catch((e) => void say(String(e), { title: "Marv", kind: "error" }));
      return;
    }
    if (viaOpen) open.send(line);
    else if (talk === lead) own.send(line);
    else {
      own.startWith(lead, line);
      setTalk(lead);
    }
  }

  async function guard(action: () => Promise<string | void>) {
    try {
      const said = await action();
      if (said) setNotice(said);
    } catch (e) {
      void say(String(e), { title: "Marv", kind: "error" });
    }
  }

  async function end() {
    if (!(await ask(`End team ${team.name}? Its rules are revoked, and so are its chats' "keep working while I'm away" grants: the chats stay as normal chats (with their roles and write paths) and ask before messaging each other.`, { title: "Marv", kind: "warning" })))
      return;
    await guard(async () => {
      await desktop.endTeam(folder, team.name);
      onEnded();
    });
  }

  async function stop() {
    if (rule === undefined) return;
    if (!(await ask(`Stop every chat team ${team.name}'s rule covers (every chat under this folder, not only the team's)? Their turns stop, and they stay paused until you resume.`, { title: "Marv", kind: "warning" }))) return;
    await guard(async () => `${await desktop.stopTeam(rule)} stopped.`);
  }

  return (
    <section className="team-view" aria-label={`Team ${team.name}`}>
      <div className="team-view-head">
        <h2 className="place-h">Team {team.name}</h2>
        <span className="place-buttons">
          <button className="btn" disabled={rule === undefined} onClick={() => void stop()} title="Pause and stop every chat the team's rule covers">
            Stop team
          </button>
          <button className="btn" disabled={rule === undefined} onClick={() => void guard(async () => `${await desktop.resumeTeam(rule!)} resumed.`)} title="Every paused chat the team's rule covers runs again">
            Resume team
          </button>
          <button className="btn plain" onClick={() => void end()} title="Revoke the team's rules; the chats stay">
            End team
          </button>
        </span>
      </div>
      {rule === undefined && <p className="help warn">This team's rule is gone (revoked or expired): its chats ask before messaging each other.</p>}
      <form
        className="team-tell"
        onSubmit={(e) => {
          e.preventDefault();
          tell();
        }}
      >
        <input
          value={text}
          placeholder={leadChat ? `Tell the team (goes to ${leadChat.title}, as you)` : "Tell the team"}
          aria-label="Tell the team"
          disabled={!lead}
          onChange={(e) => setText(e.target.value)}
        />
        <button className="btn primary" disabled={!text.trim() || !lead}>
          Send
        </button>
      </form>
      {reply && (
        <p className="team-reply" title={leadChat ? `${leadChat.title}'s reply` : undefined}>
          <b>{leadChat?.title ?? "lead"}:</b> {reply}
        </p>
      )}
      {!viaOpen && own.error && <p className="notice error">{own.error}</p>}
      <div className="team-roster">
        {cards.map((c) => (
          <div key={c.id} className={`team-roster-card${c.waiting > 0 ? " waiting" : ""}`}>
            <div className="team-roster-line">
              <ToneMarv tone={c.tone} />
              <span className="role-badge">{c.role || "no role"}</span>
              <b className="grow">{c.title}</b>
              <button className="icon-btn" title="Open this chat" onClick={() => onOpenChat(c.id)}>
                ↗
              </button>
            </div>
            <span className="place-meta">
              {c.word}
              {c.model ? ` · ${c.model}` : ""}
            </span>
            {c.waiting > 0 && (
              <button className="btn primary team-approve" onClick={() => onOpenChat(c.id)}>
                {c.waiting} waiting for your approval: open
              </button>
            )}
            <span className="place-meta">{c.writes ? `writes only ${c.writes.join(", ")}` : "writes as its role says"}</span>
            {c.editing.length > 0 && <span className="help warn">also edited by another chat: {c.editing.join(", ")}</span>}
            {c.processes.map((p) => (
              <span key={p.name} className="place-meta">
                runs {p.name}
                {p.url && (
                  <>
                    {" · "}
                    <button className="link mono" title="Open in your browser" onClick={() => void desktop.openUrl(p.url!).catch((e) => void say(String(e), { title: "Marv" }))}>
                      {p.url}
                    </button>
                  </>
                )}
              </span>
            ))}
            {c.report && <span className="team-report" title="Its latest report to the lead">report · {c.report}</span>}
          </div>
        ))}
      </div>
      {notice && <p className="notice">{notice}</p>}
      <h2 className="place-h spaced">Board</h2>
      {team.rules.length > 0 ? <Board rules={team.rules} /> : <p className="faint place-empty">No rule, so no board.</p>}
    </section>
  );
}
