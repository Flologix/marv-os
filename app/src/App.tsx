import { listen } from "@tauri-apps/api/event";
import { ask, open } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Activity, awayText } from "./components/Activity";
import { Apps } from "./components/Apps";
import { ChatView } from "./components/ChatView";
import { Dock } from "./components/Dock";
import { Home } from "./components/Home";
import { Onboarding } from "./components/Onboarding";
import { AccountGate } from "./components/AccountGate";
import { Martian } from "./components/Martian";
import { SettingsPage } from "./components/SettingsPage";
import { PlacesBar } from "./components/SpacesBar";
import { Sidebar } from "./components/Sidebar";
import { Teams } from "./components/Teams";
import {
  type AppInfo,
  type ChatGrant,
  type ChatRecord,
  type RoleEntry,
  type ChatSettings,
  type LiveChat,
  type Model,
  type UsageView,
  desktop,
  isDesktop,
  loadModels,
  type TerminalAgent,
} from "./lib/backend";
import { Icon } from "./components/icons";
import { forget, useChat } from "./lib/chat";
import { planModels } from "./lib/plan";
import { teamSteps } from "./lib/teams";
import { type DockPrefs, HOME_FOLDER, type Place, chatTone, dockFor, isHomeChat, isPlace, needsYou, relUnder, samePath } from "./lib/places";
import { applyTheme, storedTheme } from "./lib/theme";
import { say } from "./lib/dialog";
import { brainSkipped } from "./lib/setup";
import { MAC } from "./lib/platform";

const SETTINGS_KEY = "marv-settings";
const DEFAULT_SETTINGS: ChatSettings = { model: "z-ai/glm-5.3-flash", maxSteps: 50, yolo: true, mcpOff: [] };

function stored<T>(key: string, fallback: T): T {
  try {
    return JSON.parse(localStorage.getItem(key) ?? "null") ?? fallback;
  } catch {
    return fallback;
  }
}

function keep(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {}
}

function useSettings() {
  const [settings, setSettings] = useState<ChatSettings>(() => ({ ...DEFAULT_SETTINGS, ...stored(SETTINGS_KEY, DEFAULT_SETTINGS) }));
  const [models, setModels] = useState<Model[]>([]);
  // On the Marv plan's key, only the plan's models are offered (`planModels`).
  const reloadModels = () =>
    Promise.all([loadModels(), isDesktop ? desktop.planStatus(false).catch(() => null) : Promise.resolve(null)]).then(
      ([list, plan]) => setModels(planModels(list, plan)),
      () => setModels([]),
    );
  // Nothing picked here yet: start on the model `marv` uses in a terminal (~/.marv/config.json), so both agree.
  useEffect(() => {
    if (!isDesktop || stored<ChatSettings | null>(SETTINGS_KEY, null)) return;
    void desktop.marvConfig().then(
      (c) => c.model && c.model.includes("/") && setSettings((s) => ({ ...s, model: c.model! })),
      () => {},
    );
  }, []);
  useEffect(() => keep(SETTINGS_KEY, settings), [settings]);
  useEffect(() => {
    void reloadModels();
  }, []);
  return { settings, setSettings, models, reloadModels };
}

export default function App() {
  return isDesktop ? <Desktop /> : <Browser />;
}

/**
 * Marv desktop (kernel #53): places across the top (Home, Folders, Teams, Activity, Apps, Settings); in Folders, the
 * space's chats by folder on the left, the chat in the middle and its dock on the right.
 */
function Desktop() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [place, setPlaceState] = useState<Place>(() => {
    const saved = stored<unknown>("marv-place", "folders");
    return isPlace(saved) ? saved : "folders";
  });
  const [folder, setFolder] = useState(() => stored("marv-folder", ""));
  const [chatId, setChatId] = useState<string | null>(null);
  const [chats, setChats] = useState<ChatRecord[]>([]);
  const [collapsed, setCollapsed] = useState(false);
  const { settings, setSettings, models, reloadModels } = useSettings();
  // A new provider or sign-in (the Marv plan's key installed, or gone): the model menu is offered again for it.
  useEffect(() => {
    if (info) void reloadModels();
  }, [info?.provider, info?.ready, info?.signedIn]); // eslint-disable-line react-hooks/exhaustive-deps
  const [reload, setReload] = useState(0);
  // "first": the first-run onboarding; "plan" (#159): its first step again, to set up a plan or change provider;
  // "rerun": Settings' onboarding again (the provider step, then the rest).
  const [setup, setSetup] = useState<false | "first" | "plan" | "rerun">(false);
  const away = useAway();
  // The active space's switched-off apps go with every chat in it.
  const appsOff = info?.spaces.find((s) => s.id === info.active)?.appsOff ?? [];
  const appsOffKey = appsOff.join(",");
  // A team member keeps its own model (the New team dialog's): its record's, or the one picked in it since.
  const [pinned, setPinned] = useState<Record<string, string>>({});
  const openRecord = chats.find((c) => c.id === chatId);
  const teamModel = chatId && openRecord?.team ? (pinned[chatId] ?? openRecord.settings?.model) : undefined;
  const teamStepsOf = chatId && openRecord?.team ? (openRecord.settings?.maxSteps ?? 0) : null;
  const chatSettings = useMemo(
    () => ({
      ...settings,
      model: teamModel ?? settings.model,
      // A team member keeps its own step limit (New team's, `teamSteps`), not the one your other chats use.
      maxSteps: teamStepsOf === null ? settings.maxSteps : teamSteps(Math.max(settings.maxSteps, teamStepsOf)),
      mcpOff: appsOffKey ? appsOffKey.split(",") : [],
    }),
    [settings, appsOffKey, teamModel, teamStepsOf],
  );
  const setChatSettings = (next: ChatSettings) => {
    if (chatId && openRecord?.team) {
      setPinned((p) => ({ ...p, [chatId]: next.model }));
      setSettings({ ...next, model: settings.model });
    } else setSettings(next);
  };
  const [newTeam, setNewTeam] = useState(false);
  const starting = useRef(false);
  const chat = useChat(chatId, chatSettings, reload);

  const setPlace = useCallback((next: Place) => {
    setPlaceState(next);
    keep("marv-place", next);
  }, []);

  // First run (or a step still missing): the onboarding, through to connecting apps.
  // A provider that goes missing later (signed out in Settings) doesn't throw the person back here: only a first run
  // does. "Skip for now" at the brain step (someone who only wants Claude Code or Codex) counts as set up.
  const everReady = useRef(false);
  useEffect(() => {
    if (info?.ready) everReady.current = true;
    const brainMissing = info && !info.ready && !everReady.current && !brainSkipped();
    if (info && (!info.root || brainMissing || (!info.signedIn && !info.accountAsked))) setSetup("first");
  }, [info]);

  useEffect(() => {
    applyTheme(storedTheme());
  }, []);

  // Kernel #41: this launch closed an earlier window that had stopped responding: said once.
  useEffect(() => {
    desktop.watchdogNotice().then(
      (text) => text && void say(text, { title: "Marv" }),
      () => {},
    );
  }, []);

  // Opened as `marv-desktop <dir>`: that folder; else where the last session left off.
  useEffect(() => {
    desktop.takeLaunch().then(
      (launch) => {
        if (!launch) return desktop.info().then(setInfo);
        setInfo(launch.info);
        setFolder(launch.folder);
        setChatId(null);
        setPlace("folders");
      },
      (e) => {
        void say(String(e), { title: "Marv", kind: "error" });
        desktop.info().then(setInfo);
      },
    );
  }, [setPlace]);

  useEffect(() => keep("marv-folder", folder), [folder]);

  // The running chats (#155): the sidebar marks them, Home counts the ones waiting for you.
  const [live, setLive] = useState<LiveChat[]>([]);
  useEffect(() => {
    const poll = () => desktop.liveChats().then(setLive, () => {});
    void poll();
    const timer = setInterval(poll, 2000);
    return () => clearInterval(timer);
  }, []);

  // Grants and budgets: what needs you besides approvals, and today's spend in the bar.
  const [grants, setGrants] = useState<ChatGrant[]>([]);
  const [usage, setUsage] = useState<UsageView | null>(null);
  useEffect(() => {
    const poll = () => {
      desktop.chatGrants().then((g) => setGrants(g.grants), () => setGrants([]));
      desktop.usageView().then(setUsage, () => setUsage(null));
    };
    poll();
    const timer = setInterval(poll, 10_000);
    return () => clearInterval(timer);
  }, []);

  // Every chat (the sidebar shows the space's; Home and "needs an answer" any space's).
  // `chatsLoaded`: the list came from the backend (a failed fetch shows none, which isn't "every chat is gone").
  const chatsLoaded = useRef(false);
  const refreshChats = useCallback(() => {
    desktop.chats().then(
      (list) => ((chatsLoaded.current = true), setChats(list)),
      () => ((chatsLoaded.current = false), setChats([])),
    );
  }, []);
  const waitingIds = live.filter((c) => c.waiting > 0).map((c) => c.id).join(",");
  useEffect(() => {
    if (info?.root) refreshChats();
  }, [info?.root, refreshChats, waitingIds]);

  // The open chat vanished (deleted here, by Marv or a team): its view would be dead. Only a chat the list has shown
  // once can vanish, so a chat just created (the list refreshes a moment later) isn't dropped.
  const seenChats = useRef(new Set<string>());
  useEffect(() => {
    if (!chatId || !chatsLoaded.current) return;
    if (chats.some((c) => c.id === chatId)) seenChats.current.add(chatId);
    else if (seenChats.current.has(chatId)) {
      seenChats.current.delete(chatId);
      setChatId(null);
    }
  }, [chats, chatId]);

  // A click on a notification: that chat, wherever it is.
  const jump = useRef<(target: ChatRecord) => void>(() => {});
  useEffect(() => {
    const off = listen<string>("open-chat", ({ payload: id }) => {
      desktop.chats().then((list) => {
        const target = list.find((c) => c.id === id);
        if (target) jump.current(target);
      }, () => {});
    });
    return () => void off.then((stop) => stop());
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "b") {
        // In a terminal plain Ctrl+B is the agent's (Claude Code's background key, tmux, readline); ⌘B is the app's.
        if (e.ctrlKey && !e.metaKey && e.target instanceof Element && e.target.closest(".xterm")) return;
        e.preventDefault();
        setCollapsed((c) => !c);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // The dock (kernel #53): per chat, as it was left there, else as the last one was.
  const dockKey = chatId ?? `draft:${folder}`;
  const [dockSaved, setDockSaved] = useState<Record<string, DockPrefs>>(() => stored("marv-dock", {}));
  const [dockLast, setDockLast] = useState<DockPrefs>(() => stored("marv-dock-last", { open: true, tab: "files" }));
  const dock = dockFor(dockSaved, dockKey, dockLast);
  function setDock(next: DockPrefs) {
    const saved = { ...dockSaved, [dockKey]: next };
    // Only chats are remembered one by one; a draft takes the last chat's.
    if (!chatId) delete saved[dockKey];
    setDockSaved(saved);
    setDockLast(next);
    keep("marv-dock", saved);
    keep("marv-dock-last", next);
  }

  // The new chat's role (marv_session::roles): picked in the draft, fixed once its first message makes the chat.
  const [roles, setRoles] = useState<RoleEntry[]>([]);
  const [roleProblems, setRoleProblems] = useState<string[]>([]);
  const [draftRole, setDraftRole] = useState("");
  useEffect(() => {
    if (chatId) return;
    // A Home chat takes no role: roles bound a project's chats (`send` leaves the role out there).
    if (folder === HOME_FOLDER) return;
    let alive = true;
    desktop.listRoles(folder).then(
      ([list, problems]) => {
        if (!alive) return;
        setRoles(list);
        setRoleProblems(problems);
        setDraftRole((r) => (list.some((x) => x.name === r) ? r : ""));
      },
      () => alive && (setRoles([]), setRoleProblems([])),
    );
    return () => {
      alive = false;
    };
  }, [chatId, folder]);
  const picked = roles.find((r) => r.name === draftRole);
  // The coding agents a new chat can run in its terminal (Marv, Claude Code, Codex, OpenCode…), installed or not.
  const [agents, setAgents] = useState<TerminalAgent[]>([]);
  // Bumped when Install has put a new agent here: the list is asked for again.
  const [agentsFound, setAgentsFound] = useState(0);
  useEffect(() => {
    if (chatId || !isDesktop) return;
    let alive = true;
    desktop.terminalAgents().then((list) => alive && setAgents(list), () => alive && setAgents([]));
    return () => {
      alive = false;
    };
  }, [chatId, agentsFound]);
  // The agent Install is running for (one at a time), and what the last one said.
  const [installing, setInstalling] = useState<string | null>(null);
  const [installNote, setInstallNote] = useState<{ text: string; tone: "info" | "error" } | null>(null);
  async function installAgent(agent: TerminalAgent) {
    if (!agent.winget || installing) return;
    setInstalling(agent.id);
    setInstallNote(null);
    try {
      await desktop.installPackage(agent.winget);
      setInstallNote({ text: `${agent.name} is installed. Sign in once in its chat, as it asks.`, tone: "info" });
      setAgentsFound((n) => n + 1);
    } catch (e) {
      setInstallNote({ text: String(e), tone: "error" });
    } finally {
      setInstalling(null);
    }
  }
  // Windows without git: said once at the top, with Install Git (git joins the PATH of programs started after it, so
  // Marv OS is restarted to use it).
  const [gitMissing, setGitMissing] = useState(false);
  const [gitNote, setGitNote] = useState<string | null>(null);
  const [gitInstalling, setGitInstalling] = useState(false);
  useEffect(() => {
    if (isDesktop) desktop.gitMissing().then(setGitMissing, () => {});
  }, []);
  async function installGit() {
    setGitInstalling(true);
    try {
      const said = await desktop.installPackage("Git.Git");
      // winget's last lines are noise here; the Mac's own installer says what to do next (it isn't "installed" yet).
      setGitNote(/restart marv os/i.test(said) ? said : "Git is installed. Restart Marv OS so it finds it.");
    } catch (e) {
      setGitNote(String(e));
    } finally {
      setGitInstalling(false);
    }
  }
  // Its own worktree (phase 2): offered in a git folder, ticked when another agent's chat already works here.
  const [inGit, setInGit] = useState(false);
  // The person's tick, for the folder it was made in.
  const [draftTree, setDraftTree] = useState<{ at: string; on: boolean } | null>(null);
  useEffect(() => {
    if (chatId || !isDesktop) return;
    let alive = true;
    desktop.folderInGit(folder).then((on) => alive && setInGit(on), () => alive && setInGit(false));
    return () => {
      alive = false;
    };
  }, [chatId, folder]);

  if (!info) return null;
  if (setup === "plan" || setup === "rerun")
    return (
      <Onboarding
        info={info}
        again
        onInfo={(next) => {
          setInfo(next);
          void reloadModels();
          setSetup(setup === "rerun" ? "first" : false);
        }}
        onModel={(model) => setSettings({ ...settings, model })}
        onFinish={() => setSetup(setup === "rerun" ? "first" : false)}
      />
    );
  if (setup)
    return (
      <Onboarding info={info} onInfo={setInfo} onModel={(model) => setSettings({ ...settings, model })} onFinish={() => setSetup(false)} />
    );
  const rootName = info.rootName ?? "marv";
  const root = info.root ?? "";
  const hereFull = folder ? `${root}/${folder}` : root;
  const neighbour = chats.some((c) => !!c.harness && samePath(c.folder, hereFull) && !c.worktree);
  const ownTree = inGit && (draftTree?.at === folder ? draftTree.on : neighbour);
  // A new chat is its agent's terminal: pick the agent and it starts, here (in a Home chat inside Marv's sandbox, where there is one).
  const atHomeDraft = folder === HOME_FOLDER;
  // The agent you started last comes first: the one you'd pick again.
  const lastAgent = stored<string | null>("marv-last-agent", null);
  const installedAgents = agents.filter((a) => a.installed).sort((a, b) => Number(b.id === lastAgent) - Number(a.id === lastAgent));
  // Not here yet but one click away (winget, on Windows).
  const installableAgents = agents.filter((a) => !a.installed && a.winget);
  const agentPick = (
    <div className="agent-start">
      <span className="agent-start-label">Start a chat with</span>
      <div className="agent-tiles">
        {installedAgents.map((a) => (
          <button key={a.id} className="agent-tile" onClick={() => void startAgent(a)} title={`${a.name} in this chat's terminal: its own interface, models and prompts`}>
            <Icon name="terminal" size={14} />
            <span className="agent-tile-name">{a.name}</span>
          </button>
        ))}
        {installableAgents.map((a) => (
          <button
            key={a.id}
            className="agent-tile install"
            disabled={!!installing}
            onClick={() => void installAgent(a)}
            title={`Install ${a.name} with winget, from its publisher (${a.install})`}
          >
            <Icon name="plus" size={14} />
            <span className="agent-tile-name">{installing === a.id ? `Installing ${a.name}…` : `Install ${a.name}`}</span>
          </button>
        ))}
      </div>
      {installNote && <p className={`notice ${installNote.tone}`}>{installNote.text}</p>}
      <p className="role-about">
        {!atHomeDraft
          ? "It runs as in your own terminal, with its own models and settings."
          : info.sandbox
            ? "It runs in your home folder inside Marv's sandbox: passwords, keys, logins and Marv's settings stay hidden from it, and it asks before every change."
            : "It runs in your home folder with no sandbox (this PC has none for agents yet): it can read everything you can, and it asks before every change."}
      </p>
    </div>
  );

  // Another space: its own folder, chats and apps, back where you left it (renaming one keeps the view), or at
  // `target`.
  function toSpace(next: AppInfo, target?: { folder: string; chatId: string }) {
    const was = info?.active;
    setInfo(next);
    if (next.active === was) return;
    const views = stored<Record<string, { folder: string; chatId: string | null }>>("marv-space-views", {});
    if (was) views[was] = { folder, chatId };
    keep("marv-space-views", views);
    if (target) {
      setFolder(target.folder);
      setChatId(target.chatId);
      return;
    }
    const view = next.active ? views[next.active] : undefined;
    setFolder(view?.folder ?? "");
    setChatId(null);
    // Its chat again, unless it was deleted since.
    const id = view?.chatId;
    if (id)
      desktop.chats(view.folder).then(
        (list) => list.some((c) => c.id === id) && setChatId(id),
        () => {},
      );
  }

  // A chat anywhere: its space (the one whose folder holds it most closely), its folder, the chat, in Folders.
  function jumpTo(target: ChatRecord) {
    if (isHomeChat(target, info!.home)) {
      setPlace("folders");
      setFolder(HOME_FOLDER);
      setChatId(target.id);
      return;
    }
    const space = info!.spaces
      .filter((s) => relUnder(target.folder, s.root) !== null)
      .sort((a, b) => b.root.length - a.root.length)[0];
    if (!space) return;
    setPlace("folders");
    const rel = relUnder(target.folder, space.root) ?? "";
    if (space.id === info!.active && samePath(info!.root ?? "", space.root)) {
      setFolder(rel);
      setChatId(target.id);
    } else {
      desktop.switchSpace(space.id).then((next) => toSpace(next, { folder: rel, chatId: target.id }), (e) => void say(String(e), { title: "Marv" }));
    }
  }
  jump.current = jumpTo;
  const openChat = (id: string) => {
    const target = chats.find((c) => c.id === id);
    if (target) jumpTo(target);
    else
      desktop.chats().then((list) => {
        const found = list.find((c) => c.id === id);
        if (found) jumpTo(found);
      }, () => {});
  };
  const inSpace = (c: ChatRecord) => relUnder(c.folder, root) !== null;
  const waiting = chats.filter((c) => c.id !== chatId && !inSpace(c) && !isHomeChat(c, info.home) && live.some((l) => l.id === c.id && l.waiting > 0));
  const needs = needsYou(live, chats, grants, usage?.budgets ?? []);
  const running = live.filter((l) => l.busy || l.waiting > 0).length;
  const everything = usage?.budgets.find((b) => b.scope === "global");


  function selectFolder(rel: string) {
    if (rel !== folder) setChatId(null);
    setFolder(rel);
  }

  async function send(text: string) {
    if (chatId) return chat.send(text);
    // A draft: the chat exists from its first message, titled by it.
    const title = text.split("\n")[0].slice(0, 60);
    const created = await desktop.createChat(
      folder,
      title,
      (folder !== HOME_FOLDER && draftRole) || null,
      null,
      false,
    );
    chat.startWith(created.id, text);
    setChatId(created.id);
    refreshChats();
  }

  /** A new chat with `agent` in this folder: it opens on the agent's terminal. */
  async function startAgent(agent: TerminalAgent) {
    // A second click while the first is still creating would make a second chat and agent.
    if (starting.current) return;
    starting.current = true;
    try {
      await startAgentOnce(agent);
    } finally {
      starting.current = false;
    }
  }
  async function startAgentOnce(agent: TerminalAgent) {
    const harness = agent.id === "marv" ? null : agent.id;
    keep("marv-last-agent", agent.id);
    // Windows has no outer sandbox yet: an agent's Home chat there reads everything in your user folder, as it would
    // started in a terminal. Said once, before the first.
    if (harness && folder === HOME_FOLDER && navigator.userAgent.includes("Windows") && !stored("marv-home-unsandboxed-ok", false)) {
      const yes = await ask(
        `${agent.name} in Home can read everything in your user folder (your keys, .ssh, browser data): Windows has no Marv sandbox yet to hide them. Marv still asks before every change it makes.\n\nStart it in Home anyway? A project's folder keeps it to that folder.`,
        { title: "Marv", kind: "warning", okLabel: "Start in Home", cancelLabel: "Cancel" },
      );
      if (!yes) return;
      keep("marv-home-unsandboxed-ok", true);
    }
    try {
      const created = await desktop.createChat(
        folder,
        agent.name,
        (folder !== HOME_FOLDER && draftRole) || null,
        harness,
        folder !== HOME_FOLDER ? ownTree : false,
      );
      setChatId(created.id);
      refreshChats();
    } catch (e) {
      void say(String(e));
    }
  }

  async function changeRoot() {
    const path = await open({ directory: true, title: "Choose your Marv folder" });
    if (typeof path !== "string") return;
    try {
      setInfo(await desktop.setRoot(path));
      setFolder("");
      setChatId(null);
    } catch (e) {
      void say(String(e), { title: "Marv", kind: "error" });
    }
  }

  // Double-click: into that folder.
  async function enter(rel: string) {
    try {
      setInfo(await desktop.enterFolder(rel));
      setFolder("");
      setChatId(null);
    } catch (e) {
      void say(String(e), { title: "Marv", kind: "info" });
    }
  }

  // ↑: the Marv folder's parent becomes the Marv folder; the selected folder stays selected.
  async function rootUp() {
    try {
      const was = rootName;
      setInfo(await desktop.rootUp());
      setFolder(folder ? `${was}/${folder}` : was);
      setChatId(null);
    } catch (e) {
      void say(String(e), { title: "Marv", kind: "info" });
    }
  }

  const current = chats.find((c) => c.id === chatId);
  const atHome = folder === HOME_FOLDER;
  // Teams, Apps and Settings work on the space's folders: from a Home chat, the space's top folder.
  const spaceFolder = atHome ? "" : folder;
  const folderName = spaceFolder ? spaceFolder.split("/").pop()! : rootName;
  const newHomeChat = () => (setPlace("folders"), setFolder(HOME_FOLDER), setChatId(null));
  const crumbs = (
    <>
      <span title={atHome ? info.home : undefined}>{atHome ? "Home" : folder || rootName}</span>
      {chatId ? (
        <span>
          / <b>{current?.title}</b>
          {current?.role && <span className="faint" title="This chat's role, fixed when it started"> · as {current.role}</span>}
          {current?.worktree && <span className="faint"> · ⎇ {current.worktree.branch}</span>}
        </span>
      ) : (
        <span>
          / <b>new chat</b>
        </span>
      )}
    </>
  );
  const chatState = chatId ? chatTone(live.find((l) => l.id === chatId)) : undefined;

  return (
    <div className="frame">
      <PlacesBar place={place} onPlace={setPlace} needs={needs.length} todayUsd={usage ? usage.todayUsd : null} />
      {gitMissing && (
        <div className="act-away" role="status">
          {gitNote ? (
            <span>{gitNote}</span>
          ) : (
            <span>
              <b>Git isn't installed.</b> Turns and undo, worktree chats and the Git tab need it.
            </span>
          )}
          {!gitNote && (
            <button className="btn" disabled={gitInstalling} onClick={() => void installGit()}>
              {gitInstalling ? "Installing Git…" : "Install Git"}
            </button>
          )}
          <button className="icon-btn" title="Dismiss" onClick={() => setGitMissing(false)}>
            ×
          </button>
        </div>
      )}
      {away.card && place !== "home" && (
        <div className="act-away" role="status">
          <b>While you were away</b> · {awayText(away.card)}
          <button className="btn" onClick={() => setPlace("home")}>
            Home
          </button>
          <button className="icon-btn" title="Dismiss" onClick={away.dismiss}>
            ×
          </button>
        </div>
      )}
      {place === "home" && (
        <Home needs={needs} running={running} todayUsd={usage ? usage.todayUsd : null} capUsd={everything?.capUsd ?? null} away={away.card} onOpenChat={openChat} onPlace={setPlace}
          homeChats={chats.filter((c) => isHomeChat(c, info.home))}
          recent={chats.filter((c) => !isHomeChat(c, info.home))}
          live={live}
          onNewHomeChat={newHomeChat}
        />
      )}
      {place === "teams" && (
        <AccountGate info={info} onInfo={setInfo} feature="Teams" pitch="A lead and workers in one folder, handing each other tasks and reporting back, while you do something else.">
          <Teams
            folder={spaceFolder}
            folderName={folderName}
            newTeam={newTeam}
            onNewTeam={setNewTeam}
            settings={{ ...settings, mcpOff: chatSettings.mcpOff }}
            models={models}
            hasKey={info.hasKey}
            chats={chats}
            live={live}
            open={{ id: chatId, send: chat.send, items: chat.items }}
            onOpenChat={openChat}
            onChatsChanged={refreshChats}
          />
        </AccountGate>
      )}
      {place === "activity" && <Activity onOpen={openChat} chats={chats} todayUsd={usage ? usage.todayUsd : null} />}
      {place === "apps" && (
        <Apps
          folder={spaceFolder}
          folderName={folderName}
          servers={chat.servers}
          chatOpen={chatId !== null && chat.status === "ready"}
          chatId={chatId}
          chatTitle={current?.title}
          mcpOff={chatSettings.mcpOff}
          onToggleServer={(name, on) =>
            void desktop
              .setAppsOff(on ? appsOff.filter((n) => n !== name) : [...appsOff, name])
              .then(setInfo, (e) => void say(String(e), { title: "Marv", kind: "error" }))
          }
          onServerAction={chat.mcp}
          onServersChanged={() => setReload((r) => r + 1)}
          onBack={() => setPlace("settings")}
          info={info}
          onInfo={setInfo}
        />
      )}
      {place === "settings" && (
        <SettingsPage
          info={info}
          onInfo={setInfo}
          settings={settings}
          onSettings={setSettings}
          models={models}
          onReloadModels={() => void reloadModels()}
          usage={usage}
          onUsage={setUsage}
          folder={spaceFolder}
          folderName={folderName}
          onSetup={(which) => setSetup(which === "plan" ? "plan" : "rerun")}
          onApps={() => setPlace("apps")}
          onOpenChat={(id) => (refreshChats(), openChat(id))}
        />
      )}
      <div className={`shell${collapsed ? " collapsed" : ""}${dock.open ? " docked" : ""}${dock.open && dock.tab === "git" ? " wide-dock" : ""}`} hidden={place !== "folders"}>
        <Sidebar
          key={info.root}
          info={info}
          onInfo={toSpace}
          rootName={rootName}
          onRootUp={() => void rootUp()}
          onEnter={(rel) => void enter(rel)}
          folder={folder}
          onFolder={selectFolder}
          home={info.home}
          chats={chats}
          chatId={chatId}
          onChat={(id, rel) => (setFolder(rel), setChatId(id))}
          onNewChat={(rel) => (setFolder(rel), setChatId(null))}
          onNewTeam={(rel) => (selectFolder(rel), setNewTeam(true), setPlace("teams"))}
          onChatsChanged={refreshChats}
          live={live}
          onStop={(id) =>
            void desktop
              .stopChat(id)
              .then(() => desktop.liveChats().then(setLive))
              .catch((e) => void say(String(e), { title: "Marv", kind: "error" }))
          }
          onDeleted={(id) => id === chatId && setChatId(null)}
          waiting={waiting}
          onJump={jumpTo}
          onGuest={() => setPlace("activity")}
          onChangeRoot={() => void changeRoot()}
        />
        <ChatView
          key={chatId ?? `draft:${folder}`}
          onGone={() => (chatId && forget(chatId), setChatId(null), refreshChats())}
          onSwitched={() => (chatId && forget(chatId), setReload((r) => r + 1), refreshChats())}
          chat={chat}
          crumbs={crumbs}
          state={chatState}
          settings={chatSettings}
          onSettings={setChatSettings}
          models={models}
          hasKey={info.hasKey}
          onSetupPlan={info.offerZai || info.offerKimi || info.offerChatgpt ? () => setSetup("plan") : undefined}
          onSend={(text) => void send(text)}
          chatId={chatId}
          onOpenSettings={() => setPlace("settings")}
          left={
            <button className="icon-btn" title={`${collapsed ? "Show" : "Hide"} folders (${MAC ? "⌘B" : "ctrl+b"})`} onClick={() => setCollapsed((c) => !c)}>
              {collapsed ? "»" : "«"}
            </button>
          }
          right={
            <button className="icon-btn" aria-pressed={dock.open} title={dock.open ? "Hide files and git" : "Show files and git"} onClick={() => setDock({ ...dock, open: !dock.open })}>
              {dock.open ? "»" : "«"}
            </button>
          }
          empty={
            atHome ? (
              <section className="empty">
                <Martian size={56} className="toon" />
                <span className="where">Home · {info.home}</span>
                <h1>A chat for anything on this computer</h1>
                <p className="faint">Find a file, tidy Downloads, check a setting, answer a question. For a project, start a chat in its folder instead.</p>
                {agentPick}
              </section>
            ) : (
            <section className="empty">
              <Martian size={56} className="toon" />
              <span className="where">
                {rootName}
                {folder && ` / ${folder}`}
              </span>
              <h1>New chat here</h1>
              {agentPick}
              {((inGit && !atHomeDraft) || roles.length > 0) && (
                <details className="draft-more" open={!!draftRole || (neighbour && ownTree) || undefined}>
                  <summary>More options</summary>
              {inGit && !atHomeDraft && (
                <label className="role-pick" title="A new git branch from this folder's last commit, in a folder of its own; merge it when you're happy with it">
                  <input type="checkbox" checked={ownTree} onChange={(e) => setDraftTree({ at: folder, on: e.target.checked })} /> Work in its own worktree
                  {neighbour && <span className="faint"> (another agent's chat works in this folder)</span>}
                </label>
              )}
              {roles.length > 0 && (
                <label className="role-pick">
                  Start as{" "}
                  <select value={draftRole} onChange={(e) => setDraftRole(e.target.value)} title="A role bounds the chat for its whole life: its tools, the files it may write, whose mesh tasks it takes">
                    <option value="">no role</option>
                    {roles.map((r) => (
                      <option key={r.name} value={r.name}>
                        {r.name} ({r.source})
                      </option>
                    ))}
                  </select>
                </label>
              )}
              {picked && <p className="role-about">{roleAbout(picked)}</p>}
              {roleProblems.length > 0 && <p className="faint">Role notes: {roleProblems.join(" · ")}</p>}
                </details>
              )}
            </section>
            )
          }
        />
        {dock.open && <Dock key={`dock:${dockKey}`} chatId={chatId} folder={folder} chat={chat} team={!!chats.find((c) => c.id === chatId)?.team} tab={dock.tab} onTab={(tab) => setDock({ ...dock, tab })} />}
      </div>
    </div>
  );
}

/**
 * "While you were away" (runtime-kernel §20.3): when the window comes back after 10 minutes or more hidden or
 * unfocused, one card sums the interval from the kernel's counts, with the spend that went by meanwhile.
 */
function useAway() {
  const [card, setCard] = useState<import("./lib/backend").Away | null>(null);
  useEffect(() => {
    // The kernel keeps when you left and today's spend then; it says the card when you're back (marv_session::live).
    const leave = () => void desktop.awayLeave().catch(() => {});
    const back = () => void desktop.awayBack().then((a) => a && setCard(a), () => {});
    const onVisibility = () => (document.hidden ? leave() : back());
    window.addEventListener("blur", leave);
    window.addEventListener("focus", back);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.removeEventListener("blur", leave);
      window.removeEventListener("focus", back);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);
  return { card, dismiss: () => setCard(null) };
}

/** In a plain browser: one chat with marv-web, in a workspace of its own on the server. */
function Browser() {
  const [chatId, setChatId] = useState(() => stored<string | null>("marv-web-chat", null) ?? crypto.randomUUID());
  const { settings, setSettings, models } = useSettings();
  const chat = useChat(chatId, settings);
  useEffect(() => keep("marv-web-chat", chatId), [chatId]);
  return (
    <ChatView
      chat={chat}
      crumbs={
        <>
          <b>marv</b>
          <button className="icon-btn" title="New chat" onClick={() => setChatId(crypto.randomUUID())}>
            +
          </button>
        </>
      }
      settings={settings}
      onSettings={setSettings}
      models={models}
      onSend={chat.send}
      empty={
        <section className="empty">
          <Martian size={56} className="toon" />
          <h1>What should Marv build?</h1>
          <p>This chat runs in a fresh workspace on the marv-web server.</p>
        </section>
      }
    />
  );
}

/** One line on what a role may do, for the new-chat picker. */
function roleAbout(r: RoleEntry): string {
  const tools = r.tools ? `tools: ${r.tools.join(", ") || "none"}` : r.except.length ? `every tool but ${r.except.join(", ")}` : "every tool";
  const writes = r.writes === null ? "writes anywhere" : r.writes.length ? `writes only ${r.writes.join(", ")}` : "changes no files";
  const tasks = r.takesTasksFrom.length ? ` · takes tasks from ${r.takesTasksFrom.join(", ")}` : "";
  return `${r.description} — ${tools} · ${writes}${tasks}`;
}
