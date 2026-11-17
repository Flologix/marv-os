import { ask } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { harnessTitle } from "../lib/harness";
import { type AgentRow, type AppInfo, type ChatRecord, type Entry, type LiveChat, desktop } from "../lib/backend";
import { forget } from "../lib/chat";
import { forgetTerm } from "./TermView";
import { HOME_FOLDER, chatTone, groupChats, isHomeChat, relUnder } from "../lib/places";
import { splitByTeam, teamHeader } from "../lib/teams";
import { Icon } from "./icons";
import { SpaceMenu } from "./SpacesBar";
import { ToneMarv } from "./Martian";

type Editing = { mode: "new"; parent: string } | { mode: "rename"; rel: string; name: string } | null;

const join = (parent: string, name: string) => (parent ? `${parent}/${name}` : name);
const parentOf = (rel: string) => (rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "");

/**
 * Kernel #53, Folders: one tree. Home on top, then the space's top folder: each folder's chats (state dot, role and
 * agent badges, a worktree's branch mark; teams grouped), its terminal Marvs, then its subfolders, indented with a guide
 * line. Folders with chats open by themselves; a collapsed one shows how many chats it holds and whether one needs you.
 */
export function Sidebar({
  info,
  onInfo,
  rootName,
  folder,
  onFolder,
  home,
  chats,
  chatId,
  onChat,
  onNewChat,
  onNewTeam,
  onChatsChanged,
  onDeleted,
  onChangeRoot,
  onRootUp,
  onEnter,
  live,
  onStop,
  waiting,
  onJump,
  onGuest,
}: {
  info: AppInfo;
  onInfo: (info: AppInfo) => void;
  rootName: string;
  folder: string;
  onFolder: (rel: string) => void;
  /** The home folder: its chats (Home chats) are listed on top, outside the space. */
  home: string;
  /** Every chat (the sidebar keeps the space's). */
  chats: ChatRecord[];
  chatId: string | null;
  onChat: (id: string, folder: string) => void;
  onNewChat: (folder: string) => void;
  /** The New team dialog for a folder (in Teams). */
  onNewTeam: (folder: string) => void;
  onChatsChanged: () => void;
  /** A chat was deleted here: the app closes it if it's the open one. */
  onDeleted: (id: string) => void;
  onChangeRoot: () => void;
  onRootUp: () => void;
  onEnter: (rel: string) => void;
  /** Chats running now, here or elsewhere (#155). */
  live: LiveChat[];
  onStop: (id: string) => void;
  /** Chats elsewhere waiting for an approval. */
  waiting: ChatRecord[];
  onJump: (chat: ChatRecord) => void;
  /** A guest agent (a terminal Marv in a folder here): to Activity. */
  onGuest: () => void;
}) {
  const [children, setChildren] = useState<Record<string, Entry[]>>({});
  // Folders the person opened or closed; besides these, a folder with chats inside (and the one new chats go to) is
  // open by itself ([`shown`]).
  const [open, setOpen] = useState<Set<string>>(new Set([""]));
  const [closed, setClosed] = useState<Set<string>>(new Set());
  const [editing, setEditing] = useState<Editing>(null);
  // A chat's right-click menu (where it opened), and the chat being renamed in place. A new name is only its title:
  // its id, and so its mesh name and wires, stay.
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("click", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);
  const renameChat = (id: string, title: string) => {
    setRenaming(null);
    const was = chats.find((c) => c.id === id)?.title ?? "";
    if (!title.trim() || title.trim() === was) return;
    void run(() => desktop.renameChat(id, title.trim()).then(onChatsChanged), []);
  };
  const [error, setError] = useState<string | null>(null);
  const [guests, setGuests] = useState<AgentRow[]>([]);
  // The open folders as of the last render, for the refresh timer.
  const openKey = useRef("");
  const root = info.root ?? "";

  // Guest agents: terminal Marvs working in a folder of this space, from the kernel's Activity rows.
  useEffect(() => {
    const look = () =>
      desktop.activity().then(
        (v) => setGuests(v.activity.rows.filter((r) => r.kind === "terminal" && relUnder(r.folder, root) !== null)),
        () => setGuests([]),
      );
    void look();
    const timer = setInterval(look, 5000);
    return () => clearInterval(timer);
  }, [root]);

  // Folders that couldn't be read (deleted or renamed outside the app): left alone, not read again every few seconds
  // with the same error. Opening one by hand tries again.
  const failed = useRef(new Set<string>());
  const load = useCallback(async (rel: string) => {
    try {
      const entries = await desktop.listDir(rel);
      failed.current.delete(rel);
      setChildren((c) => ({ ...c, [rel]: entries }));
      setError(null);
    } catch (e) {
      setError(String(e));
      // The space's own folder stays: nothing else to show without it.
      if (rel === "") return;
      failed.current.add(rel);
      setOpen((o) => {
        if (!o.has(rel)) return o;
        const next = new Set(o);
        next.delete(rel);
        return next;
      });
      setChildren((c) => {
        if (!(rel in c)) return c;
        const { [rel]: _gone, ...rest } = c;
        return rest;
      });
    }
  }, []);

  // Marv changes folders too: refresh what's open every few seconds.
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") for (const rel of openKey.current.split("|")) if (!failed.current.has(rel)) void load(rel);
    }, 4000);
    return () => clearInterval(timer);
  }, [load]);

  function toggle(rel: string) {
    const wasOpen = isOpen(rel);
    setOpen((o) => {
      const next = new Set(o);
      if (wasOpen) next.delete(rel);
      else next.add(rel);
      return next;
    });
    setClosed((c) => {
      const next = new Set(c);
      if (wasOpen) next.add(rel);
      else next.delete(rel);
      return next;
    });
    if (!wasOpen) {
      failed.current.delete(rel);
      void load(rel);
    }
  }

  async function run(action: () => Promise<unknown>, refresh: string[]) {
    setError(null);
    try {
      await action();
      for (const rel of refresh) await load(rel);
    } catch (e) {
      setError(String(e));
    }
  }

  async function finishEdit(value: string) {
    const edit = editing;
    setEditing(null);
    const name = value.trim();
    if (!edit || !name) return;
    if (edit.mode === "new") {
      await run(async () => {
        const made = await desktop.createFolder(edit.parent, name);
        setOpen((o) => new Set(o).add(edit.parent));
        onFolder(made);
      }, [edit.parent]);
    } else if (name !== edit.name) {
      await run(async () => {
        const renamed = await desktop.rename(edit.rel, name);
        if (folder === edit.rel || folder.startsWith(`${edit.rel}/`)) onFolder(renamed + folder.slice(edit.rel.length));
        onChatsChanged();
      }, [parentOf(edit.rel)]);
    }
  }

  async function remove(rel: string) {
    if (!(await ask(`Move ${rel} to the trash?`, { title: "Marv", kind: "warning" }))) return;
    await run(async () => {
      await desktop.trash(rel);
      if (folder === rel || folder.startsWith(`${rel}/`)) onFolder(parentOf(rel));
    }, [parentOf(rel)]);
  }

  async function removeChat(chat: ChatRecord) {
    if (!(await ask(`Delete the chat "${chat.title}"? The folder's files stay.`, { title: "Marv", kind: "warning" }))) return;
    setError(null);
    try {
      await desktop.deleteChat(chat.id);
    } catch (e) {
      setError(String(e));
      return;
    }
    forget(chat.id);
    // Its kept terminals (the agent's and the Git tab's) end with it.
    forgetTerm(`chat:${chat.id}`);
    forgetTerm(`git:${chat.id}`);
    onDeleted(chat.id);
    onChatsChanged();
  }

  const groups = groupChats(chats, root, rootName, folder);
  const homeChats = chats.filter((c) => isHomeChat(c, home)).sort((a, b) => b.created - a.created);
  const states = new Map(live.map((l) => [l.id, l]));
  const guestRel = (g: AgentRow) => relUnder(g.folder, root) ?? "";
  const chatsIn = new Map(groups.map((g) => [g.rel, g.chats]));

  // Folders that hold chats (and the folder new chats go to) open by themselves, with their parents, unless the person
  // closed them: no chat hides behind a closed folder it didn't choose.
  const auto = new Set<string>();
  for (const rel of [...groups.map((g) => g.rel), folder === HOME_FOLDER ? "" : folder]) {
    const parts = rel ? rel.split("/") : [];
    for (let i = 0; i <= parts.length; i++) auto.add(parts.slice(0, i).join("/"));
  }
  const shown = [...new Set([...open, ...auto])].filter((rel) => !closed.has(rel) || open.has(rel));
  const shownKey = shown.join("|");
  useEffect(() => {
    openKey.current = shownKey;
  }, [shownKey]);
  function isOpen(rel: string) {
    return shown.includes(rel);
  }
  // Every open folder's subfolders, loaded once it opens.
  useEffect(() => {
    for (const rel of shownKey.split("|")) {
      if (!(rel in children) && !failed.current.has(rel)) void load(rel);
    }
  }, [shownKey, children, load]);

  /** The chats in `rel` and every folder below it: a collapsed folder's count and state. */
  function under(rel: string) {
    const list = groups.filter((g) => g.rel === rel || rel === "" || g.rel.startsWith(`${rel}/`)).flatMap((g) => g.chats);
    const tones = list.map((c) => chatTone(states.get(c.id)).tone);
    const tone = tones.includes("waiting") ? "waiting" : tones.includes("working") ? "working" : null;
    return { count: list.length, tone };
  }

  /** One chat's row (a team member's with its role badge). */
  function chatRow(chat: ChatRecord, rel: string, inTeam: boolean) {
    const state = states.get(chat.id);
    const { tone, word } = chatTone(state);
    return (
      <div
        key={chat.id}
        className={`nav-row chat${chat.id === chatId ? " selected" : ""}`}
        onClick={() => renaming !== chat.id && onChat(chat.id, rel)}
        onContextMenu={(e) => (e.preventDefault(), setMenu({ id: chat.id, x: e.clientX, y: e.clientY }))}
        title={`${chat.title} · ${word} · right-click to rename`}
      >
        <span className="nav-lead">
          <ToneMarv tone={tone} title={state?.cantWake ? `Asleep · a message couldn't wake it: ${state.cantWake}` : word} />
        </span>
        {/* In a team its name says who it is ("lead", "w-marv"); a name that's only "<folder>-<role>" shows as the role. */}
        {renaming === chat.id ? (
          <input
            className="nav-rename"
            autoFocus
            defaultValue={chat.title}
            spellCheck={false}
            aria-label="Chat name"
            onClick={(e) => e.stopPropagation()}
            onFocus={(e) => e.target.select()}
            onBlur={(e) => renameChat(chat.id, e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") renameChat(chat.id, e.currentTarget.value);
              if (e.key === "Escape") setRenaming(null);
            }}
          />
        ) : (
          <span className="nav-name">{inTeam && chat.role && (!chat.title || chat.title.endsWith(`-${chat.role}`)) ? chat.role : chat.title}</span>
        )}
        {chat.harness && (
          <span className="nav-badge agent" title={`Run by your own ${harnessTitle(chat.harness)}`}>
            {harnessTitle(chat.harness)}
          </span>
        )}
        {chat.worktree && (
          <span className="nav-mark" title={`Works in a worktree: ${chat.worktree.branch}`}>
            <Icon name="branch" size={12} />
          </span>
        )}
        {chat.private && (
          <span className="nav-mark" title="Private: other chats can't read it">
            <Icon name="lock" size={12} />
          </span>
        )}
        <span className="nav-actions" onClick={(e) => e.stopPropagation()}>
          {state?.paused && (
            <button
              className="icon-btn"
              title="Paused: mail and timers wait. Resume it"
              onClick={() => void run(async () => {
                await desktop.chatSignal(chat.id, "resume");
                onChatsChanged();
              }, [])}
            >
              <Icon name="play" size={12} />
            </button>
          )}
          {state && chat.id !== chatId && (
            <button className="icon-btn" title="Stop: it leaves the mesh (opening it again continues it)" onClick={() => onStop(chat.id)}>
              <Icon name="stop" size={12} />
            </button>
          )}
          <button
            className={`icon-btn${chat.private ? " active" : ""}`}
            title={chat.private ? "Private: other chats can't read it (click to let rules allow reads again)" : "Make private: other chats can't read it"}
            onClick={() => void run(async () => {
              await desktop.setChatPrivate(chat.id, !chat.private);
              onChatsChanged();
            }, [])}
          >
            <Icon name={chat.private ? "lock" : "unlock"} size={12} />
          </button>
          <button className="icon-btn danger" title="Delete chat" onClick={() => void removeChat(chat)}>
            <Icon name="x" size={12} />
          </button>
        </span>
      </div>
    );
  }

  /** What's inside folder `rel`: its chats (teams grouped), its guests, then its subfolders. */
  function inside(rel: string): React.ReactNode[] {
    const out: React.ReactNode[] = [];
    const split = splitByTeam(chatsIn.get(rel) ?? []);
    for (const t of split.teams) {
      out.push(
        <div key={`team:${rel}:${t.name}`} className="nav-group">
          <div className="nav-row team" title={`${teamHeader(t.name, t.chats)} · started together by New team; Teams shows the roster`}>
            <span className="nav-lead">
              <Icon name="team" size={13} />
            </span>
            <span className="nav-name">{t.name}</span>
            <span className="nav-count">{t.chats.length}</span>
          </div>
          <div className="nav-children">{t.chats.map((chat) => chatRow(chat, rel, true))}</div>
        </div>,
      );
    }
    out.push(...split.rest.map((chat) => chatRow(chat, rel, false)));
    for (const g of guests.filter((g) => guestRel(g) === rel)) {
      out.push(
        <div key={`guest:${g.name}:${g.pid}`} className="nav-row chat guest" title={`A terminal Marv here (pid ${g.pid}): its approvals are its own. Click for Activity.`} onClick={onGuest}>
          <span className="nav-lead">
            <Icon name="terminal" size={12} />
          </span>
          <span className="nav-name">{g.title || g.name}</span>
          <span className="nav-badge">terminal</span>
        </div>,
      );
    }
    if (editing?.mode === "new" && editing.parent === rel) {
      out.push(<EditRow key={`new:${rel}`} initial="" onDone={finishEdit} />);
    }
    for (const entry of (children[rel] ?? []).filter((e) => e.dir)) {
      out.push(folderNode(join(rel, entry.name), entry.name));
    }
    return out;
  }

  /** A folder's row and, when open, what's inside it (indented, with a guide line). */
  function folderNode(rel: string, name: string, top = false): React.ReactNode {
    if (editing?.mode === "rename" && editing.rel === rel) {
      return <EditRow key={`edit:${rel}`} initial={name} onDone={finishEdit} />;
    }
    const folderOpen = isOpen(rel);
    const { count, tone } = under(rel);
    const body = folderOpen ? inside(rel) : [];
    return (
      <div key={`folder:${rel}`} className="nav-group">
        <div
          className={`nav-row folder${folder === rel ? " selected" : ""}${top ? " top" : ""}`}
          title={folder === rel ? "New chats start here · double-click to make it the top folder" : "Click: new chats start here · double-click to make it the top folder"}
          onClick={() => onFolder(rel)}
          onDoubleClick={() => !top && onEnter(rel)}
        >
          <button
            className={`nav-chevron${folderOpen ? " open" : ""}`}
            aria-label={folderOpen ? `Close ${name}` : `Open ${name}`}
            onClick={(e) => {
              e.stopPropagation();
              toggle(rel);
            }}
          >
            <Icon name="chevron" size={12} />
          </button>
          <span className="nav-lead folder-icon">
            <Icon name={folderOpen ? "folderOpen" : "folder"} size={14} />
          </span>
          <span className="nav-name">{name}</span>
          {!folderOpen && count > 0 && (
            <span className="nav-count" title={`${count} chat${count === 1 ? "" : "s"} inside`}>
              {tone && <ToneMarv tone={tone} />}
              {count}
            </span>
          )}
          <span className="nav-actions" onClick={(e) => e.stopPropagation()}>
            <button className="icon-btn" title={`New chat in ${name}`} onClick={() => onNewChat(rel)}>
              <Icon name="plus" size={12} />
            </button>
            <button className="icon-btn" title={`New team in ${name}: chats that work together, each with a role`} onClick={() => onNewTeam(rel)}>
              <Icon name="team" size={12} />
            </button>
            <button className="icon-btn" title="New folder inside" onClick={() => (setOpen((o) => new Set(o).add(rel)), void load(rel), setEditing({ mode: "new", parent: rel }))}>
              <Icon name="folder" size={12} />
            </button>
            {!top && (
              <>
                <button className="icon-btn" title="Rename" onClick={() => setEditing({ mode: "rename", rel, name })}>
                  <Icon name="pencil" size={12} />
                </button>
                <button className="icon-btn danger" title="Move to trash" onClick={() => void remove(rel)}>
                  <Icon name="trash" size={12} />
                </button>
              </>
            )}
          </span>
        </div>
        {folderOpen && (body.length > 0 ? <div className="nav-children">{body}</div> : <div className="nav-children"><p className="nav-empty">empty: + to start a chat here</p></div>)}
      </div>
    );
  }

  return (
    <aside className="side">
      <div className="side-head">
        <SpaceMenu info={info} onInfo={onInfo} />
        <button className="icon-btn" title="Up a level: the Marv folder's parent becomes the Marv folder" onClick={onRootUp}>
          <Icon name="up" size={13} />
        </button>
        <button className="icon-btn" aria-label="New chat" title={`New chat in ${folder === HOME_FOLDER ? "Home" : folder || rootName}`} onClick={() => onNewChat(folder)}>
          <Icon name="plus" size={14} />
        </button>
      </div>
      <div className="nav-tree">
        {waiting.map((chat) => (
          <button key={chat.id} className="waiting-elsewhere" onClick={() => onJump(chat)} title={chat.folder}>
            <span className="dot" />
            <span className="title">{chat.title}</span>
            <span className="faint">needs an answer</span>
          </button>
        ))}
        <div className="nav-group">
          <div
            className={`nav-row folder home${folder === HOME_FOLDER ? " selected" : ""}`}
            title={`${home}: a chat for anything on this computer. It reads your home folder and asks before every change.`}
            onClick={() => onNewChat(HOME_FOLDER)}
          >
            <span className="nav-chevron spacer" />
            <span className="nav-lead folder-icon">
              <Icon name="home" size={14} />
            </span>
            <span className="nav-name">Home</span>
            {homeChats.length > 0 && <span className="nav-count">{homeChats.length}</span>}
            <span className="nav-actions" onClick={(e) => e.stopPropagation()}>
              <button className="icon-btn" title="New Home chat: anything on this computer" aria-label="New Home chat" onClick={() => onNewChat(HOME_FOLDER)}>
                <Icon name="plus" size={12} />
              </button>
            </span>
          </div>
          {homeChats.length > 0 && <div className="nav-children">{homeChats.map((chat) => chatRow(chat, HOME_FOLDER, false))}</div>}
        </div>
        <div className="nav-divider" />
        {folderNode("", rootName, true)}
        {error && <p className="notice error" style={{ padding: "6px 12px" }}>{error}</p>}
        {/* Seen in a simulation (2026-11-08): with "New folder" only in the folder tree, a project landed in the Marv folder itself. */}
        <button className="new-folder" title={`A folder in ${rootName} for a new project: its chats start there`} onClick={() => (setOpen((o) => new Set(o).add("")), setEditing({ mode: "new", parent: "" }))}>
          <Icon name="plus" size={12} /> New folder
        </button>
      </div>
      <div className="side-foot">
        <span className="grow" />
        <button className="icon-btn" title="Choose another Marv folder" onClick={onChangeRoot}>
          <Icon name="swap" size={13} />
        </button>
      </div>
      {menu && (
        <div className="context-menu" role="menu" style={{ left: menu.x, top: menu.y }} onClick={(e) => e.stopPropagation()}>
          <button role="menuitem" onClick={() => (setRenaming(menu.id), setMenu(null))}>
            <Icon name="pencil" size={12} /> Rename
          </button>
        </div>
      )}
    </aside>
  );
}

function EditRow({ initial, onDone }: { initial: string; onDone: (value: string) => void }) {
  const [value, setValue] = useState(initial);
  return (
    <div className="nav-row folder editing">
      <span className="nav-chevron spacer" />
      <span className="nav-lead folder-icon">
        <Icon name="folder" size={14} />
      </span>
      <input
        autoFocus
        value={value}
        placeholder="folder name"
        spellCheck={false}
        onChange={(e) => setValue(e.target.value)}
        onBlur={() => onDone(value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") onDone(value);
          if (e.key === "Escape") onDone("");
        }}
        onFocus={(e) => e.target.select()}
      />
    </div>
  );
}
