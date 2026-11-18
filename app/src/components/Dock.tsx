import { useCallback, useEffect, useRef, useState } from "react";
import { type Changes, type Entry, type MeshRule, type WriteMark, desktop } from "../lib/backend";
import { say } from "../lib/dialog";
import type { useChat } from "../lib/chat";
import { DOCK_TABS, type DockTab, HOME_FOLDER, markFor } from "../lib/places";
import { Board } from "./Board";
import { TermView, forgetTerm } from "./TermView";
import { ruleName } from "./Teams";

type Chat = ReturnType<typeof useChat>;

const join = (parent: string, name: string) => (parent ? `${parent}/${name}` : name);

/**
 * Kernel #53: the chat's dock, on the right, per chat. Its files (with who is editing them, from the write sets), its
 * changes (the git diff, read-only for now), its team's board, the processes it started, and its context. Each tab
 * reads the desktop's commands while it shows; nothing here decides anything.
 */
export function Dock({
  chatId,
  folder,
  chat,
  team,
  tab: asked,
  onTab,
}: {
  /** The chat; null for a draft (its folder's files only). */
  chatId: string | null;
  /** The folder relative to the space's top folder (a draft's files). */
  folder: string;
  chat: Chat;
  /** The chat is one of a team's: its board shows. */
  team: boolean;
  tab: DockTab;
  onTab: (tab: DockTab) => void;
}) {
  const tabs = DOCK_TABS.filter((t) => t !== "board" || (team && chatId));
  const tab = tabs.includes(asked) ? asked : "files";
  return (
    <aside className="dock-panel" aria-label="Dock">
      <div className="dock-tabs" role="tablist">
        {tabs.map((t) => (
          <button key={t} role="tab" aria-selected={tab === t} className={tab === t ? "active" : ""} onClick={() => onTab(t)}>
            {t}
          </button>
        ))}
      </div>
      <div className={`dock-body${tab === "git" ? " flush" : ""}`}>
        {tab === "files" && (chatId || folder !== HOME_FOLDER ? <Files chatId={chatId} folder={folder} /> : <Draft />)}
        {tab === "git" && (chatId || folder !== HOME_FOLDER ? <GitTab key={chatId ?? folder} chatId={chatId} folder={folder} /> : <Draft />)}
        {tab === "board" && (chatId ? <BoardTab chatId={chatId} meshName={chat.meshName} /> : <Draft />)}
      </div>
    </aside>
  );
}

/**
 * The folder's own lazygit, in the dock: each folder is a project, and its lazygit keeps running while you move between
 * its chats and tabs (the next look attaches to it, at once). Falls back to the read-only changes without lazygit.
 */
function GitTab({ chatId, folder }: { chatId: string | null; folder: string }) {
  const termId = useRef<number | null>(null);
  const [state, setState] = useState<"starting" | "running" | "ended" | { error: string }>("starting");
  // A new value starts a new lazygit (after it ended, or "fresh").
  const [run, setRun] = useState(0);
  const key = `git:${chatId ?? folder}`;
  const fresh = () => {
    if (termId.current !== null) void desktop.gitTerminalClose(termId.current);
    forgetTerm(key);
    setState("starting");
    setRun((n) => n + 1);
  };
  const popOut = () => void desktop.gitTerminalPopOut(chatId, folder).catch((e) => void say(String(e)));
  const name = folder === HOME_FOLDER ? "Home" : folder.split("/").filter(Boolean).pop() || "Marv folder";
  // lazygit runs only in a git repository (elsewhere it quits at once): asked first, and offered to start one.
  const [repo, setRepo] = useState<boolean | null>(null);
  useEffect(() => {
    let alive = true;
    desktop.gitRepoHere(chatId, folder).then((r) => alive && setRepo(r), () => alive && setRepo(true));
    return () => {
      alive = false;
    };
  }, [chatId, folder, run]);
  const init = () =>
    void desktop.gitInitHere(chatId, folder).then(
      () => (setRepo(true), setRun((n) => n + 1)),
      (e) => void say(String(e)),
    );
  if (repo === false) {
    return (
      <div className="term-exited small">
        <p className="faint">{name} isn't a git repository yet, so there's nothing for lazygit to show.</p>
        <button className="term-again" onClick={init}>
          Start a git repository here
        </button>
      </div>
    );
  }
  if (repo === null) return <p className="faint dock-note">Looking…</p>;

  if (typeof state === "object") {
    return (
      <>
        <p className="notice dock-note">{state.error}</p>
        {chatId && <ChangesTab chatId={chatId} />}
      </>
    );
  }
  return (
    <div className="git-tab">
      <div className="git-bar">
        <span className="git-where" title="lazygit, in this folder: keys as in your terminal (? for help, q quits)">
          lazygit · {name}
        </span>
        <button title="A fresh lazygit for this folder" onClick={fresh}>
          ↻
        </button>
        <button title="Open lazygit in your own terminal window" onClick={popOut}>
          ⤢
        </button>
      </div>
      {state === "ended" ? (
        <div className="term-exited small">
          <p className="faint">lazygit exited.</p>
          <button className="term-again" autoFocus onClick={fresh}>
            Start lazygit again
          </button>
        </div>
      ) : (
        <TermView
          cacheKey={key}
          run={run}
          open={(cols, rows, onData) => desktop.gitTerminalOpen(chatId, folder, cols, rows, onData)}
          onState={setState}
          onId={(id) => (termId.current = id)}
        />
      )}
      {state === "starting" && <p className="faint git-starting">Starting lazygit…</p>}
    </div>
  );
}

function Draft() {
  return <p className="faint dock-note">Start a chat with an agent: its files and git show up here once it runs.</p>;
}

/** The chat's own folder (its worktree when it has one), a level at a time; marked files say who wrote them lately. */
function Files({ chatId, folder }: { chatId: string | null; folder: string }) {
  const [children, setChildren] = useState<Record<string, Entry[]>>({});
  const [open, setOpen] = useState<Set<string>>(new Set([""]));
  const [marks, setMarks] = useState<WriteMark[]>([]);
  const [error, setError] = useState<string | null>(null);

  // A folder deleted or renamed outside the app is dropped from what's open, not read again with the same error
  // every few seconds; the next successful read clears the message.
  const load = useCallback(
    (rel: string) =>
      (chatId ? desktop.chatFiles(chatId, rel) : desktop.listDir(join(folder, rel))).then(
        (entries) => (setChildren((c) => ({ ...c, [rel]: entries })), setError(null)),
        (e) => {
          setError(String(e));
          if (rel === "") return;
          setOpen((o) => {
            const next = new Set(o);
            next.delete(rel);
            return next;
          });
          setChildren((c) => {
            const { [rel]: _gone, ...rest } = c;
            return rest;
          });
        },
      ),
    [chatId, folder],
  );

  useEffect(() => {
    setChildren({});
    setOpen(new Set([""]));
    setError(null);
    void load("");
  }, [load]);

  useEffect(() => {
    if (!chatId) return setMarks([]);
    const look = () => desktop.chatWrites(chatId).then(setMarks, () => setMarks([]));
    void look();
    const timer = setInterval(look, 5000);
    return () => clearInterval(timer);
  }, [chatId]);

  // Marv changes files: what's open is read again every few seconds.
  useEffect(() => {
    const timer = setInterval(() => document.visibilityState === "visible" && open.forEach((rel) => void load(rel)), 5000);
    return () => clearInterval(timer);
  }, [open, load]);

  function toggle(rel: string) {
    setOpen((o) => {
      const next = new Set(o);
      if (next.has(rel)) next.delete(rel);
      else {
        next.add(rel);
        void load(rel);
      }
      return next;
    });
  }

  function rows(parent: string, depth: number): React.ReactNode[] {
    return (children[parent] ?? []).flatMap((entry) => {
      const rel = join(parent, entry.name);
      const mark = markFor(rel, entry.dir, marks);
      const row = (
        <div
          key={rel}
          className={`file-row${entry.dir ? " dir" : ""}${mark ? " marked" : ""}`}
          style={{ paddingLeft: 12 + depth * 14 }}
          onClick={entry.dir ? () => toggle(rel) : undefined}
          title={mark ? `${mark.by} wrote ${entry.dir ? "in here" : "this"} in the last 30 minutes` : rel}
        >
          <span className="name">
            {entry.name}
            {entry.dir ? "/" : ""}
          </span>
          {mark && <span className={`by${mark.mine ? " mine" : ""}`}>{mark.by}</span>}
        </div>
      );
      return entry.dir && open.has(rel) ? [row, ...rows(rel, depth + 1)] : [row];
    });
  }

  const others = marks.filter((m) => !m.mine).length;
  return (
    <>
      {marks.length > 0 && (
        <p className="dock-note">
          {marks.length} file{marks.length === 1 ? "" : "s"} written lately{others ? `, ${others} by another chat` : ""}
        </p>
      )}
      {rows("", 0)}
      {children[""]?.length === 0 && <p className="faint dock-note">An empty folder.</p>}
      {error && <p className="notice error dock-note">{error}</p>}
    </>
  );
}

const STATUS_MARK: Record<string, string> = { modified: "M", added: "A", deleted: "D", renamed: "R", binary: "B", untracked: "?" };

/** The chat's changes: the git diff of its folder (or its worktree's branch since it began). Read-only for now. */
function ChangesTab({ chatId }: { chatId: string }) {
  const [changes, setChanges] = useState<Changes | null>(null);
  const [shown, setShown] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => desktop.chatChanges(chatId).then((c) => (setChanges(c), setError(null)), (e) => setError(String(e))), [chatId]);

  useEffect(() => {
    void load();
    const timer = setInterval(load, 10_000);
    return () => clearInterval(timer);
  }, [load]);

  const toggle = (path: string) =>
    setShown((s) => {
      const next = new Set(s);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  if (error) return <p className="notice error dock-note">{error}</p>;
  if (!changes) return <p className="faint dock-note">Looking…</p>;
  if (changes.problem) return <p className="faint dock-note">{changes.problem}</p>;
  return (
    <>
      <div className="dock-note dock-head">
        <span>
          {changes.files.length === 0 ? "No changes" : `${changes.files.length + changes.more_files} file${changes.files.length === 1 ? "" : "s"}`}{" "}
          {changes.against === "HEAD" ? "not committed" : `since ${changes.against} (its branch)`}
        </span>
        <button className="link" onClick={() => void load()}>
          refresh
        </button>
      </div>
      {changes.files.map((f, i) => {
        const open = shown.has(f.path) || (i === 0 && shown.size === 0);
        return (
          <div key={f.path} className="change">
            <button className="change-head" aria-expanded={open} onClick={() => toggle(f.path)} title={f.status}>
              <span className={`change-mark ${f.status}`}>{STATUS_MARK[f.status] ?? "M"}</span>
              <span className="name">{f.path}</span>
              {(f.added > 0 || f.removed > 0) && (
                <span className="counts">
                  <span className="add">+{f.added}</span> <span className="del">−{f.removed}</span>
                </span>
              )}
            </button>
            {open && f.lines.length > 0 && (
              <div className="change-lines">
                {f.lines.map((l, j) => (
                  <div key={j} className={`dl ${l.kind}`}>
                    {l.kind === "gap" ? `@@ ${l.text}` : `${l.kind === "add" ? "+" : l.kind === "del" ? "-" : " "} ${l.text}`}
                  </div>
                ))}
                {f.more > 0 && <div className="dl gap">… {f.more} more lines</div>}
              </div>
            )}
          </div>
        );
      })}
      {changes.more_files > 0 && <p className="faint dock-note">… {changes.more_files} more files</p>}
    </>
  );
}

/** The board of the team this chat is on: the rules whose chats include it (the kernel's teams, by mesh name). */
function BoardTab({ chatId, meshName }: { chatId: string; meshName: string | null | undefined }) {
  const [rules, setRules] = useState<MeshRule[] | null>(null);
  const [rule, setRule] = useState<number | null>(null);
  useEffect(() => {
    const look = async () => {
      try {
        const [view, all] = await Promise.all([desktop.activity(), desktop.meshRules()]);
        const mine = new Set(view.teams.filter((t) => (meshName && t.names.includes(meshName)) || view.activity.rows.some((r) => r.chat === chatId && t.names.includes(r.name))).map((t) => t.rule));
        setRules(all.rules.filter((r) => mine.has(r.id) && r.mode !== "watch"));
      } catch {
        setRules([]);
      }
    };
    void look();
    const timer = setInterval(() => void look(), 10_000);
    return () => clearInterval(timer);
  }, [chatId, meshName]);
  if (!rules) return <p className="faint dock-note">Looking…</p>;
  if (rules.length === 0) return <p className="faint dock-note">Not on a team: no rule covers this chat. Make one in Teams.</p>;
  const chosen = rules.find((r) => r.id === rule) ?? rules[0];
  return (
    <>
      {rules.length > 1 && (
        <div className="dock-note">
          <select value={chosen.id} onChange={(e) => setRule(Number(e.target.value))} aria-label="Rule">
            {rules.map((r) => (
              <option key={r.id} value={r.id}>
                {ruleName(r)}
              </option>
            ))}
          </select>
        </div>
      )}
      <Board rules={[chosen.id]} compact />
    </>
  );
}
