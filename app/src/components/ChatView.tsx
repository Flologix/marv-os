import { type ReactNode, useEffect, useRef, useState } from "react";
import { harnessTitle, permissionModes, permissionName } from "../lib/harness";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Components } from "react-markdown";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  type ChatGrant,
  type ChatTurn,
  type TerminalAgent,
  type ChatSettings,
  type ChatWorktree,
  type Model,
  type WorktreeStatus,
  type WriteOverlap,
  desktop,
  grantSpend,
  isDesktop,
  overlapText,
} from "../lib/backend";
import { type Answer, type ApprovalRequest, type Command, type DiffLine, type Item, knownCommands, type useChat } from "../lib/chat";
import { tokens } from "../lib/format";
import { type Question, answerText, pickedIn, splitChoices, startAt } from "../lib/choices";
import { firstImage, readImage, refusal, withPath } from "../lib/images";
import { Martian, ToneMarv } from "./Martian";
import { useChatDevices } from "./Devices";
import { Settings } from "./Settings";
import { TermView, forgetTerm } from "./TermView";
import { say } from "../lib/dialog";
import { LINUX } from "../lib/platform";
import { allowanceText, isAllowanceError, nextReset } from "../lib/plan";

// Voxtype's key is the Omarchy desktop's: named only where it exists.
const DICTATE_KEY = LINUX ? " (Super+Ctrl+X)" : "";

/** A link in a reply opens in the person's browser, never inside the app's window (which has no way back). */
const MARKDOWN_COMPONENTS: Components = {
  a: ({ href, children }) => (
    <a
      href={href}
      onClick={(e) => {
        e.preventDefault();
        if (href && /^https?:\/\//i.test(href)) void desktop.openUrl(href);
      }}
    >
      {children}
    </a>
  ),
};

type Chat = ReturnType<typeof useChat>;

export function ChatView({
  chat,
  crumbs,
  empty,
  settings,
  onSettings,
  models,
  hasKey,
  onSetupPlan,
  onSend,
  left,
  right,
  state,
  chatId,
  onOpenSettings,
  onGone,
  onSwitched,
}: {
  chat: Chat;
  crumbs: ReactNode;
  empty: ReactNode;
  settings: ChatSettings;
  onSettings: (s: ChatSettings) => void;
  models: Model[];
  hasKey?: boolean;
  /** #159: opens the plan setup (only when this build offers a plan). */
  onSetupPlan?: () => void;
  onSend: (text: string) => void;
  left?: ReactNode;
  /** Kernel #53: after the model picker (the dock toggle). */
  right?: ReactNode;
  /** Kernel #53: the chat's state as the kernel has it (working, needs you, sleeping…): the top bar's dot and word. */
  state?: { tone: string; word: string };
  /** The desktop chat's id: its grant's badge. */
  chatId?: string | null;
  /** Settings, from an error that a key there would fix (the Marv plan's allowance used up). */
  onOpenSettings?: () => void;
  /** The chat is gone (its worktree merged or discarded). */
  onGone?: () => void;
  /** The chat now runs another agent: its connection starts again. */
  onSwitched?: () => void;
}) {
  const [input, setInput] = useState("");
  // A message a budget's limit kept from being sent goes back into an empty prompt.
  useEffect(() => {
    if (chat.unsent) setInput((now) => (now.trim() === "" ? chat.unsent!.text : now));
  }, [chat.unsent]);
  const [open, setOpen] = useState(false);
  // On the desktop every chat is its agent's own terminal (Marv, Claude Code, Codex…); a draft is the agent picker.
  const terminal = isDesktop;
  const [active, setActive] = useState(0);
  const [dismissed, setDismissed] = useState(false);
  const [listening, setListening] = useState(false);
  const promptRef = useRef<HTMLTextAreaElement>(null);
  // A pasted or dropped image: Marv saves it, and its path goes in where the caret was (Marv attaches the images a
  // message names).
  const pasteAt = useRef(0);
  const [pasteNote, setPasteNote] = useState<string | null>(null);
  // Each answer goes in once: not again when the view remounts or React's dev double-mount reruns the effect.
  const pastedSeen = useRef(chat.pasted?.n ?? 0);
  useEffect(() => {
    const p = chat.pasted;
    if (!p || p.n <= pastedSeen.current) return;
    pastedSeen.current = p.n;
    if (p.error || !p.path) {
      setPasteNote(p.error ?? "The image couldn't be saved.");
      return;
    }
    const path = p.path;
    setPasteNote(null);
    setInput((now) => {
      const { text, caret } = withPath(now, pasteAt.current, path);
      requestAnimationFrame(() => promptRef.current?.setSelectionRange(caret, caret));
      return text;
    });
  }, [chat.pasted]);
  const attach = (file: File) => {
    const refused = refusal(file);
    if (refused) {
      setPasteNote(refused);
      return;
    }
    pasteAt.current = promptRef.current?.selectionStart ?? input.length;
    void readImage(file).then((image) => {
      if (!chat.pasteImage(image)) setPasteNote("The chat isn't connected yet: paste the image again in a moment.");
    });
  };
  const [grant, setGrant] = useState<ChatGrant | null>(null);
  const [overlap, setOverlap] = useState<WriteOverlap | null>(null);
  const [worktree, setWorktree] = useState<ChatWorktree | null>(null);
  // This chat's device switches (runtime-kernel §17.1), in its settings.
  const devices = useChatDevices(chatId);

  // The chat's grant, if it has one: "works while you're away · $0.40 of $2 today".
  useEffect(() => {
    if (!isDesktop || !chatId) return;
    const grants = () =>
      desktop.chatGrants().then(
        (g) => setGrant(g.grants.find((x) => x.chat === chatId) ?? null),
        () => setGrant(null),
      );
    // Runtime-kernel §14.4: another chat writing the same files here, as paths (shown, never scheduled).
    const overlaps = () =>
      desktop.writeOverlaps().then(
        (all) => setOverlap(all.find((x) => x.chat === chatId) ?? null),
        () => setOverlap(null),
      );
    const look = () => {
      void grants();
      void overlaps();
      void desktop.chatWorktree(chatId).then(setWorktree, () => setWorktree(null));
    };
    void look();
    const timer = setInterval(() => void look(), 5000);
    return () => clearInterval(timer);
  }, [chatId]);

  // Runtime-kernel §14.1: the person moves a worktree chat back to its folder (a new conversation; the branch stays).
  async function moveBack() {
    if (!chatId || !worktree) return;
    const sure = await ask(
      `Move this chat back to its folder? It starts a new conversation there. Branch ${worktree.branch} and its worktree are kept for you to merge.`,
      { title: "Marv", kind: "warning" },
    );
    if (!sure) return;
    try {
      const said = await desktop.moveChatToFolder(chatId);
      setWorktree(null);
      setOpen(false);
      await say(said, { title: "Marv" });
    } catch (e) {
      await say(String(e), { title: "Marv", kind: "error" });
    }
  }

  // Voxtype types into the focused field: the prompt, then.
  function dictate() {
    promptRef.current?.focus();
    desktop.dictate().then(
      () => setListening((l) => !l),
      () => setListening(false),
    );
  }
  const all: Command[] = chat.commands.length ? chat.commands : knownCommands();
  // The / menu: while the first word is still being typed.
  const typing = input.startsWith("/") && !input.includes(" ") && !dismissed;
  const matches = typing ? all.filter((c) => c.name.startsWith(input.slice(1).toLowerCase())).slice(0, 50) : [];

  function pick(c: Command) {
    setInput(`/${c.name} `);
    setActive(0);
  }
  const bottom = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottom.current?.scrollIntoView({ block: "end" });
  }, [chat.items]);

  const cost = chat.items.reduce((sum, it) => sum + (it.kind === "turn" ? (it.usage?.cost ?? 0) : 0), 0);
  const last = [...chat.items].reverse().find((it) => it.kind === "turn" && it.usage);
  const usage = last?.kind === "turn" ? last.usage : undefined;
  const asking = chat.items.some((it) => it.kind === "approval" && !it.decision && !it.restored);

  // The last reply's choices (marv-web's CHOICES_PROMPT), while the chat waits for the person's answer: clickable, and
  // Enter on the empty composer takes the highlighted option (the recommended one to start).
  const liveAt = (() => {
    if (chat.busy || asking) return -1;
    for (let i = chat.items.length - 1; i >= 0; i--) {
      const it = chat.items[i];
      if (it.kind === "user") return -1;
      if (it.kind === "text") return splitChoices(it.text).questions.length ? i : -1;
    }
    return -1;
  })();
  const liveItem = liveAt >= 0 ? chat.items[liveAt] : undefined;
  const liveQuestions = liveItem?.kind === "text" ? splitChoices(liveItem.text).questions : [];
  const liveKey = liveItem?.kind === "text" ? `${liveAt}:${liveItem.text.length}` : "";
  const [picking, setPicking] = useState<{ key: string; picks: (number | null)[]; hi: number[] }>({ key: "", picks: [], hi: [] });
  const sel =
    picking.key === liveKey ? picking : { key: liveKey, picks: liveQuestions.map(() => null), hi: liveQuestions.map(startAt) };
  const current = sel.picks.findIndex((p) => p === null);
  function highlight(q: number, i: number) {
    setPicking({ ...sel, hi: sel.hi.map((h, n) => (n === q ? i : h)) });
  }
  function choose(q: number, i: number) {
    const picks = sel.picks.map((p, n) => (n === q ? i : p));
    if (picks.every((p) => p !== null)) {
      onSend(answerText(liveQuestions, picks as number[]));
      setPicking({ key: "", picks: [], hi: [] });
      return;
    }
    setPicking({ ...sel, picks, hi: sel.hi.map((h, n) => (n === q ? i : h)) });
  }
  const choosing = liveQuestions.length > 0 && current >= 0;

  function submit() {
    const text = input.trim();
    // Idle with a queue a stop kept (#184): Enter on the empty composer sends the oldest.
    if (!text) {
      if (!chat.busy && chat.queued.length > 0) chat.sendQueued();
      return;
    }
    if (chat.busy) {
      // While a turn runs a message is queued for after it; a /command waits in the composer for the turn.
      if (text.startsWith("/") || chat.status !== "ready") return;
      chat.enqueue(text);
      setInput("");
      return;
    }
    onSend(text);
    setInput("");
  }

  /** Takes queued message `i` back into the composer to change it (Enter queues it again). */
  function editQueued(i?: number) {
    const text = chat.unqueue(i);
    if (text !== undefined) {
      setInput(text);
      promptRef.current?.focus();
    }
  }

  /** "Send now": while a turn runs, the message goes in at its next step. A /command waits for the turn. */
  const canSendNow = chat.busy && chat.status === "ready" && input.trim() !== "" && !input.trim().startsWith("/");
  function sendNow() {
    if (!canSendNow) return;
    if (chat.sendNow(input.trim())) setInput("");
  }

  return (
    <div className="main">
      <header className="topbar">
        {left}
        <div className="crumbs">{crumbs}</div>
        {grant && (
          <span
            className="faint"
            title="This chat keeps working while you're away: what yolo would run, nothing past it. Revoke it in Teams."
          >
            {grant.paused ? "paused" : "works while you're away"} · {grantSpend(grant)}
          </span>
        )}
        {overlapText(overlap) && (
          <span
            className="faint warn"
            title={`Written by this chat and another in the last 30 minutes:\n${overlap!.with.map((w) => `${w.name}: ${w.paths.join(", ")}`).join("\n")}\nMarv refuses an edit over a change a chat hasn't read; give each its own worktree to keep them apart.`}
          >
            {overlapText(overlap)}
          </span>
        )}
        {state ? (
          <span
            className="chat-state"
            title={chat.meshName ? `On the mesh as ${chat.meshName}: other Marvs see it (marv mesh ls) and can ask you for a wire` : undefined}
          >
            <ToneMarv tone={chat.status === "failed" ? "failed" : state.tone} />
            {chat.status === "failed" ? "failed" : chat.status === "connecting" ? "starting" : state.word}
          </span>
        ) : (
          chat.status !== "idle" && <span className={`status-dot ${chat.status}`} title={chat.status} />
        )}
        {cost > 0 && <span className="faint">${cost.toFixed(4)}</span>}
        {chat.budget.budget ? <ContextMeter used={chat.context ?? usage?.context ?? 0} budget={chat.budget.budget} /> : null}
        {terminal && chatId && worktree && <BranchChip chatId={chatId} onGone={onGone} />}
        {terminal && chatId && <TurnsChip chatId={chatId} />}
        {terminal && chatId && <PermissionChip chatId={chatId} />}
        {terminal ? (
          chatId && <AgentChip chatId={chatId} harness={chat.harness} onSwitched={onSwitched} />
        ) : chat.harness ? (
          <span className="chip">{harnessTitle(chat.harness)}</span>
        ) : (
          <button className="chip" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
            {settings.model}
          </button>
        )}
        {open && !terminal && !chat.harness && (
          <Settings
            settings={settings}
            models={models}
            hasKey={hasKey}
            onSetupPlan={onSetupPlan}
            onChange={onSettings}
            onClose={() => setOpen(false)}
            budget={chat.budget}
            onBudget={chat.status === "ready" ? chat.setBudget : undefined}
            worktree={worktree}
            onMoveBack={() => void moveBack()}
            devices={devices}
          />
        )}
        {right}
      </header>

      {terminal && chatId ? (
        <ChatTerminal key={`${chatId}:${chat.harness ?? "marv"}`} chat={chat} chatId={chatId} />
      ) : terminal ? (
        <main className="scroll">
          <div className="column">{empty}</div>
        </main>
      ) : (
      <>
      <main className="scroll">
        <div className="column">
          {chat.items.length === 0
            ? empty
            : chat.items.map((item, i) => (
                <ItemView
                  key={i}
                  item={item}
                  onAnswer={chat.answer}
                  onOpenSettings={onOpenSettings}
                  choices={
                    i === liveAt
                      ? { picks: sel.picks, hi: sel.hi, current, onHighlight: highlight, onChoose: choose, onOwn: () => promptRef.current?.focus() }
                      : { answer: nextUserText(chat.items, i) }
                  }
                />
              ))}
          {chat.busy && !asking && (
            <div className="working">
              <Martian size={12} className="toon bob" /> working
            </div>
          )}
          {chat.error && <p className="notice error">{chat.error}</p>}
          <div ref={bottom} />
        </div>
      </main>

      <footer className="dock">
        <div className="composer-wrap">
        {chat.queued.length > 0 && (
          <ul className="queued" aria-label="Queued messages">
            {chat.queued.map((text, i) => (
              <li key={i}>
                <span className="label">queued</span>
                <span className="text">{text}</span>
                <button type="button" className="link" title="Back into the composer to change it" onClick={() => editQueued(i)}>
                  edit
                </button>
                <button type="button" className="link" title="Drop it" onClick={() => chat.unqueue(i)}>
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
        {matches.length > 0 && (
          <div className="slash" role="listbox">
            {matches.map((c, i) => (
              <button
                key={c.name}
                type="button"
                role="option"
                aria-selected={i === active}
                className={i === Math.min(active, matches.length - 1) ? "active" : ""}
                onMouseDown={(e) => {
                  e.preventDefault();
                  pick(c);
                }}
              >
                <span className={`cmd${c.skill ? " skill" : ""}`}>/{c.name}</span>
                <span className="desc">{c.description}</span>
              </button>
            ))}
          </div>
        )}
        {pasteNote && (
          <div className="paste-note" role="status">
            {pasteNote}
          </div>
        )}
        <form
          className="composer"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <textarea
            ref={promptRef}
            value={input}
            rows={1}
            autoFocus
            placeholder="Ask Marv to build, fix or explain something, paste a screenshot, or type / for commands"
            onPaste={(e) => {
              const file = firstImage(e.clipboardData?.files);
              if (!file) return;
              e.preventDefault();
              attach(file);
            }}
            onDrop={(e) => {
              const file = firstImage(e.dataTransfer?.files);
              if (!file) return;
              e.preventDefault();
              attach(file);
            }}
            onChange={(e) => {
              setInput(e.target.value);
              setActive(0);
              setDismissed(false);
            }}
            onKeyDown={(e) => {
              if (matches.length > 0) {
                const current = Math.min(active, matches.length - 1);
                if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                  e.preventDefault();
                  setActive((current + (e.key === "ArrowDown" ? 1 : matches.length - 1)) % matches.length);
                  return;
                }
                if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && `/${matches[current].name}` !== input)) {
                  e.preventDefault();
                  pick(matches[current]);
                  return;
                }
                if (e.key === "Escape") {
                  setDismissed(true);
                  return;
                }
              }
              // ↑ on the empty composer: the newest queued message back to edit (#184).
              if (e.key === "ArrowUp" && input === "" && chat.queued.length > 0) {
                e.preventDefault();
                editQueued();
                return;
              }
              // The last reply's choices: ↑↓ move the highlight, Enter takes it (typing an answer of your own wins).
              if (choosing && input === "" && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
                e.preventDefault();
                const n = liveQuestions[current].options.length;
                highlight(current, (sel.hi[current] + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
                return;
              }
              if (choosing && input.trim() === "" && e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                choose(current, sel.hi[current]);
                return;
              }
              if (e.key === "Enter" && e.altKey && chat.busy) {
                e.preventDefault();
                sendNow();
                return;
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                submit();
              }
            }}
          />
          {isDesktop && (
            <button
              type="button"
              className={`mic${listening ? " on" : ""}`}
              title={listening ? `Stop dictation${DICTATE_KEY}` : `Dictate${DICTATE_KEY}`}
              onClick={dictate}
            >
              {listening ? "● rec" : "mic"}
            </button>
          )}
          {chat.busy && (
            <button
              type="button"
              className="btn primary"
              disabled={!canSendNow}
              title="Send now (alt+enter): in at Marv's next step, after the current reply and tool calls"
              onClick={sendNow}
            >
              Send now
            </button>
          )}
          {chat.busy ? (
            <button type="button" className="btn" onClick={chat.interrupt}>
              Stop
            </button>
          ) : (
            <button type="submit" className="btn primary" disabled={!input.trim() && chat.queued.length === 0}>
              Send
            </button>
          )}
        </form>
        </div>
        <div className="hint">
          <span>
            {choosing
              ? "enter picks the highlighted choice · ↑↓ to change · or type your own answer"
              : chat.busy
              ? `enter queues for after this turn · alt+enter or Send now: in at Marv's next step${chat.sending ? ` · ${chat.sending} waiting` : ""}${chat.queued.length ? " · ↑ edits the last queued" : ""}`
              : chat.held && chat.queued.length > 0
                ? "stopped: your queued messages wait · enter on an empty composer sends the next · ↑ edits it"
                : "enter to send · shift+enter for a new line · / for commands and skills"}
          </span>
          {usage?.context && usage.contextLength ? <span>context {tokens(usage.context)} / {tokens(usage.contextLength)}</span> : null}
        </div>
      </footer>
      </>
      )}
    </div>
  );
}

/** The chat: its agent's own interface in a terminal (Marv, Claude Code, Codex, OpenCode…), on the chat's conversation,
 * with its own models, providers and prompts. Marv's own cards (a mesh wire the agent's marv-mesh tools asked for, a
 * budget's limit) show over its top, leaving its size alone. Leaving the chat leaves it running; quitting the agent ends it. */
function ChatTerminal({ chat, chatId }: { chat: Chat; chatId: string }) {
  const [state, setState] = useState<"starting" | "running" | "ended" | { error: string }>("starting");
  const [run, setRun] = useState(0);
  const agent = harnessTitle(chat.harness);
  const cards = chat.items.filter((it) => it.kind === "approval" && !it.decision);
  const mail = chat.items.filter((it): it is Extract<Item, { kind: "mail" }> => it.kind === "mail");
  const [mailError, setMailError] = useState<string | null>(null);
  // An error from the page's own calls (an answer to a card that didn't go through): the terminal hides the
  // transcript it lands in, and a card that stayed put said nothing.
  const lastError = chat.items.reduce((at, it, i) => (it.kind === "notice" && it.tone === "error" ? i : at), -1);
  // The last error already shown (or dismissed), by the notice itself, not its place in the list: an index could be
  // lower in the next transcript and never show. This mounts once `useChat` knows this chat's agent, so `items` are
  // this chat's and what's in them now was seen before.
  const lastNotice = lastError >= 0 ? (chat.items[lastError] as Extract<Item, { kind: "notice" }>) : null;
  const [seenNotice, setSeenNotice] = useState(lastNotice);
  const error = lastNotice && lastNotice !== seenNotice ? lastNotice.text : null;
  const deliver = () => (setMailError(null), desktop.chatDeliverMail(chatId).catch((e) => setMailError(String(e))));
  const key = `chat:${chatId}`;
  const again = () => (forgetTerm(key), setState("starting"), setRun((n) => n + 1));
  // A Marv started before another provider was set up stays on the old one (it reads its config at start): said here.
  const [said, setSaid] = useState<string | null>(null);
  const isMarv = (chat.harness ?? "marv") === "marv";
  const providerNote = isMarv && state === "running" ? said : null;
  useEffect(() => {
    if (!isMarv || state !== "running") return;
    const look = () => desktop.chatProviderChanged(chatId).then(setSaid, () => {});
    look();
    const timer = window.setInterval(look, 5000);
    return () => window.clearInterval(timer);
  }, [chatId, isMarv, state]);
  const restart = () => void desktop.chatTerminalClose(chatId).then(() => (setSaid(null), again()), () => {});
  return (
    <main className="chat-term">
      {providerNote && (
        <div className="chat-term-cards">
          <div className="mail-card">
            <span>{providerNote}</span>
            <button className="btn" onClick={restart}>
              Restart it
            </button>
          </div>
        </div>
      )}
      {(cards.length > 0 || mail.length > 0 || mailError || error) && (
        <div className="chat-term-cards">
          {cards.map((item, i) => (
            <ItemView key={i} item={item} onAnswer={chat.answer} />
          ))}
          {mail.length > 0 && (
            // A message on a wire that doesn't wake this chat: it waits for the person, who'd otherwise never know.
            <div className="mail-card">
              <span>
                {mailFrom(mail)} {mail.reduce((n, m) => n + m.count, 0) === 1 ? "is" : "are"} waiting. Its sender was told it
                waits for you.
              </span>
              <button className="btn" onClick={() => void deliver()}>
                Deliver now
              </button>
            </div>
          )}
          {mailError && <p className="notice warn">{mailError}</p>}
          {error && (
            <p className="notice error mail-card">
              <span>{error}</span>
              <button className="btn plain" onClick={() => setSeenNotice(lastNotice)}>
                Dismiss
              </button>
            </p>
          )}
        </div>
      )}
      {typeof state === "object" ? (
        <section className="empty term-exited">
          <Martian size={48} className="toon" />
          <h1>{agent} couldn't start</h1>
          <p className="notice error">{state.error}</p>
          <button className="term-again" onClick={again}>
            Try again
          </button>
        </section>
      ) : (
        <TermView
          cacheKey={key}
          run={run}
          scrollback={5000}
          open={(cols, rows, onData) => desktop.chatTerminalOpen(chatId, cols, rows, onData)}
          onState={setState}
        />
      )}
      {state === "ended" && (
        // Its agent quit (Ctrl+C, /exit, or at once: not logged in, node missing): its last output stays above, where
        // the reason is. Its conversation is kept by the agent.
        <div className="term-ended-bar">
          <span>{agent} exited. Its conversation is saved.</span>
          <button className="term-again" autoFocus onClick={again}>
            Start {agent} again
          </button>
        </div>
      )}
      {state === "starting" && <p className="faint git-starting">Starting {agent}…</p>}
    </main>
  );
}

/** Who the waiting messages are from: "A message from flologix-ac520efc", "3 messages from a and b". */
export function mailFrom(mail: { from: string; count: number }[]): string {
  const count = mail.reduce((n, m) => n + m.count, 0);
  const names = [...new Set(mail.map((m) => m.from))];
  const who = names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}` : names[0];
  return count === 1 ? `A message from ${who}` : `${count} messages from ${who}`;
}

/** OpenRouter's 402 while the Marv plan's key is the one in use: its monthly allowance is used up (it resets on the
 * 1st, 00:00 UTC), and your own key is the way on. Any other key's 402 shows as it came. */
function AllowanceNotice({ text, onOpenSettings }: { text: string; onOpenSettings?: () => void }) {
  // When the allowance resets, if the plan's key is the one in use (else the error shows as it came).
  const [resets, setResets] = useState<string | null>(null);
  useEffect(() => {
    desktop.planStatus(true).then((s) => setResets(s.signedIn && s.keyInUse ? nextReset(new Date()) : null), () => setResets(null));
  }, []);
  if (!resets) return <p className="notice error">{text}</p>;
  return (
    <p className="notice error plan-allowance">
      <span>{allowanceText(resets)}</span>
      {onOpenSettings && (
        <button className="btn" onClick={onOpenSettings}>
          Use your own key
        </button>
      )}
    </p>
  );
}

/** Runtime-kernel §11.2: the context used of the chat's budget, where Marv compacts (not of the model's window). */
export function ContextMeter({ used, budget }: { used: number; budget: number }) {
  const share = Math.min(1, used / budget);
  const tone = share >= 0.9 ? "hot" : share >= 0.7 ? "warm" : "";
  return (
    <span className={`meter ${tone}`} title={`Context: ${used.toLocaleString()} of this chat's ${budget.toLocaleString()}-token budget. Marv compacts at the budget; set it in the chat's settings.`}>
      <span className="meter-bar" aria-hidden="true">
        <span style={{ width: `${Math.round(share * 100)}%` }} />
      </span>
      {tokens(used)} / {tokens(budget)}
    </span>
  );
}

/** The person's next message after item `i`: what a reply's choices were answered with. */
function nextUserText(items: Item[], i: number): string | undefined {
  for (let n = i + 1; n < items.length; n++) {
    const it = items[n];
    if (it.kind === "user") return it.text;
  }
  return undefined;
}

type LiveChoices = {
  picks: (number | null)[];
  hi: number[];
  current: number;
  onHighlight: (q: number, i: number) => void;
  onChoose: (q: number, i: number) => void;
  onOwn: () => void;
};
type ItemChoices = LiveChoices | { answer?: string };

function ItemView({
  item,
  onAnswer,
  onOpenSettings,
  choices,
}: {
  item: Item;
  onAnswer: (id: number, d: Answer) => void;
  onOpenSettings?: () => void;
  choices?: ItemChoices;
}) {
  switch (item.kind) {
    case "user":
      return <div className="user">{item.text}</div>;
    case "text": {
      const split = splitChoices(item.text);
      return (
        <div className="reply">
          {split.text && <Markdown remarkPlugins={[remarkGfm]} components={MARKDOWN_COMPONENTS}>{split.text}</Markdown>}
          {split.questions.map((q, n) => (
            <ChoiceCard key={n} question={q} n={n} choices={choices} />
          ))}
          <CopyButton text={item.text} />
        </div>
      );
    }
    case "thinking":
      return item.text.trim() ? (
        <details className="thinking">
          <summary>thought</summary>
          <p>{item.text.trim()}</p>
        </details>
      ) : null;
    case "tool":
      return (
        <details className={`tool${item.agent ? " sub" : ""}`}>
          <summary>
            {item.done ? <span className={`mark${item.error ? " err" : ""}`} /> : <span className="spinner" />}
            <span className="name">{item.tool}</span>
            <span className="label">{item.label}</span>
            {item.summary && <span className="summary">{item.summary}</span>}
            {item.approval === "grant" && <span className="summary">ran while you were away (grant)</span>}
          </summary>
          {item.diff && item.diff.lines.length > 0 ? (
            <Diff lines={item.diff.lines} more={item.diff.more} />
          ) : (
            item.result && <pre className="code">{item.result}</pre>
          )}
        </details>
      );
    case "approval":
      return <Approval request={item.request} decision={item.decision} reason={item.reason} restored={item.restored} onAnswer={onAnswer} />;
    case "mail":
      return null;
    case "notice":
      if (item.tone === "error" && isDesktop && isAllowanceError(item.text)) return <AllowanceNotice text={item.text} onOpenSettings={onOpenSettings} />;
      return <p className={`notice ${item.tone}`}>{item.text}</p>;
    case "output":
      return (
        <div className="output">
          <Markdown remarkPlugins={[remarkGfm]} components={MARKDOWN_COMPONENTS}>{item.text}</Markdown>
        </div>
      );
    case "turn":
      return (
        <p className="turn">
          {item.reason === "end" ? "done" : item.reason}
          {item.usage && ` · ${tokens(item.usage.promptTokens)} in · ${tokens(item.usage.completionTokens)} out · $${item.usage.cost.toFixed(4)}`}
        </p>
      );
  }
}

function Approval({
  request: r,
  decision,
  reason,
  restored,
  onAnswer,
}: {
  request: ApprovalRequest;
  decision?: string;
  reason?: string;
  /** From the saved transcript, not yet known to still wait: no buttons until the session says. */
  restored?: boolean;
  onAnswer: (id: number, d: Answer) => void;
}) {
  // #185: "Decline and say why" opens a one-line reason; the turn goes on with it. Only for a model's call
  // (`reason`): the engine says which.
  const [why, setWhy] = useState<string | null>(null);
  if (decision || restored) {
    // "expired": the session that asked ended first (Marv restarted), so there's nothing left to answer.
    const idle = !decision || decision === "expired";
    // #201: "withdrawn": nobody waits for an answer any more (another Marv asked for a wire, then hung up).
    const no = decision === "no" || decision === "declined" || decision === "withdrawn";
    const word = !decision
      ? "waiting for Marv"
      : decision === "expired"
        ? "not answered (Marv restarted)"
        : decision === "withdrawn"
          ? "no longer asked"
          : decision === "no" || decision === "declined"
            ? "declined"
            : decision === "always"
              ? "allowed for this chat"
              : "allowed";
    return (
      <p className={`decided${idle ? " idle" : no ? " no" : ""}`}>
        {word} · <span>{r.command ?? r.label}</span>
        {reason && <span> · “{reason}”</span>}
      </p>
    );
  }
  const sendWhy = () => {
    const text = (why ?? "").trim();
    if (text) onAnswer(r.id, { declined: text });
  };
  return (
    <section className="approval" aria-live="assertive">
      <span className="who">{r.agent ? `${r.agent} asks` : "Marv asks"}</span>
      <h3>{r.title}</h3>
      {r.command && <pre className="code">$ {r.command}</pre>}
      {r.diff && r.diff.length > 0 && <Diff lines={r.diff} more={0} />}
      {r.files?.map((f, i) => (
        <div className="file" key={i}>
          <h4>{f.title}</h4>
          {f.diff && f.diff.lines.length > 0 && <Diff lines={f.diff.lines} more={f.diff.more} />}
          {f.note && <span className="note">{f.note}</span>}
        </div>
      ))}
      {r.text && <pre className="code">{r.text}</pre>}
      {r.warning && <div className="warning">{r.warning}</div>}
      {r.note && <span className="note">{r.note}</span>}
      <div className="buttons">
        <button className="btn primary" autoFocus onClick={() => onAnswer(r.id, "yes")}>
          Allow
        </button>
        {!r.once && (
          <button className="btn" onClick={() => onAnswer(r.id, "always")}>
            Allow {r.scope} for this chat
          </button>
        )}
        <button className="btn plain" onClick={() => onAnswer(r.id, "no")}>
          Decline
        </button>
        {r.reason && why === null && (
          <button className="btn plain" onClick={() => setWhy("")}>
            Decline and say why
          </button>
        )}
      </div>
      {why !== null && (
        <form
          className="why"
          onSubmit={(e) => {
            e.preventDefault();
            sendWhy();
          }}
        >
          <input
            autoFocus
            value={why}
            maxLength={500} /* the engine's MAX_DECLINE_REASON; it cuts there too */
            placeholder="Why not? Marv reads this and carries on."
            onChange={(e) => setWhy(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setWhy(null);
            }}
          />
          <button type="submit" className="btn" disabled={!why.trim()}>
            Send
          </button>
        </form>
      )}
    </section>
  );
}

/**
 * One question from a reply's `choices` block: its options as buttons, the recommended one marked. Live (the chat waits
 * for this answer): click, or ↑↓ and Enter here or in the empty composer, or the option's letter while the card has
 * focus. After: the option the person picked stays marked.
 */
function ChoiceCard({ question: q, n, choices }: { question: Question; n: number; choices?: ItemChoices }) {
  const live = choices && "picks" in choices ? choices : null;
  const picked = live ? (live.picks[n] ?? -1) : pickedIn(q, choices && "answer" in choices ? choices.answer : undefined);
  const active = live !== null && live.current === n;
  const hi = live ? live.hi[n] : -1;
  return (
    <div
      className={`reply-choices${live ? " live" : ""}${active ? " active" : ""}`}
      role="radiogroup"
      aria-label={q.question || "Choose one"}
      tabIndex={active ? 0 : -1}
      onKeyDown={(e) => {
        if (!live || live.picks[n] !== null) return;
        const count = q.options.length;
        if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          live.onHighlight(n, (hi + (e.key === "ArrowDown" ? 1 : count - 1)) % count);
        } else if (e.key === "Enter") {
          e.preventDefault();
          live.onChoose(n, hi);
        } else {
          const at = q.options.findIndex((o) => o.key === e.key.toUpperCase());
          if (at >= 0 && !e.ctrlKey && !e.metaKey && !e.altKey) {
            e.preventDefault();
            live.onChoose(n, at);
          }
        }
      }}
    >
      {q.question && <p className="reply-choices-q">{q.question}</p>}
      {q.options.map((o, i) => (
        <button
          key={o.key}
          type="button"
          role="radio"
          aria-checked={picked === i}
          tabIndex={-1}
          disabled={!live || live.picks[n] !== null}
          className={`reply-choice${i === hi && live?.picks[n] === null ? " hi" : ""}${picked === i ? " picked" : ""}${o.recommended ? " rec" : ""}`}
          onMouseEnter={() => live && live.picks[n] === null && live.onHighlight(n, i)}
          onClick={() => live?.onChoose(n, i)}
        >
          <span className="reply-choice-key">{o.key}</span>
          <span className="reply-choice-text">
            <span className="reply-choice-label">
              {o.label}
              {o.recommended && <span className="pill ok">Recommended</span>}
            </span>
            {o.why && <span className="reply-choice-why">{o.why}</span>}
          </span>
        </button>
      ))}
      {active && (
        <button type="button" className="link small reply-choice-own" onClick={live.onOwn}>
          Something else? Type your own answer
        </button>
      )}
    </div>
  );
}

/** Copies a reply as Markdown, the way it was written. */
function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Older webviews: through a hidden text box.
      const area = Object.assign(document.createElement("textarea"), { value: text });
      document.body.append(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  }
  return (
    <button className="copy" title="Copy this reply" onClick={() => void copy()}>
      {copied ? "copied" : "copy"}
    </button>
  );
}

function Diff({ lines, more }: { lines: DiffLine[]; more: number }) {
  return (
    <pre className="code diff">
      {lines.map((l, i) => (
        <span key={i} className={l.kind}>
          {l.kind === "gap" ? "…" : `${l.kind === "add" ? "+" : l.kind === "del" ? "-" : " "} ${l.text}`}
        </span>
      ))}
      {more > 0 && <span className="gap">… {more} more lines</span>}
    </pre>
  );
}

/**
 * A worktree chat's branch in its header: what it has beside the folder's branch, and the person's two ways to finish
 * it, **Merge** (its work into the folder's branch, then its worktree, branch and chat go) and **Discard** (they go,
 * its work with them). Either asks first, saying what goes.
 */
/** The chat's agent, and switching it to another (in place: the one taking over is given the conversation so far, and
 * an agent it ran before continues its own). */
function AgentChip({ chatId, harness, onSwitched }: { chatId: string; harness?: string | null; onSwitched?: () => void }) {
  const [open, setOpen] = useState(false);
  const [agents, setAgents] = useState<TerminalAgent[]>([]);
  const [said, setSaid] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (open) void desktop.terminalAgents().then(setAgents, () => setAgents([]));
  }, [open]);
  const current = harness ?? "marv";
  async function to(agent: TerminalAgent) {
    const sure = await ask(
      `Switch this chat to ${agent.name}? ${harnessTitle(harness)}'s terminal ends; ${agent.name} starts in this folder and is given the conversation so far.`,
      { title: "Marv", kind: "info" },
    );
    if (!sure) return;
    setBusy(true);
    try {
      const first = await desktop.switchAgent(chatId, agent.id);
      forgetTerm(`chat:${chatId}`);
      setOpen(false);
      onSwitched?.();
      await desktop.chatTerminalType(chatId, first);
    } catch (e) {
      setSaid(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <span className="branch-chip">
      <button className="chip" aria-expanded={open} onClick={() => setOpen((o) => !o)} title="This chat's agent, in its own terminal (its models and settings are changed there); click to switch to another">
        {harnessTitle(harness)}
      </button>
      {open && (
        <div className="branch-pop" role="dialog" aria-label="Switch this chat's agent">
          <p className="muted small">Switch to another agent: it gets the conversation so far. Switching back continues where that agent left off.</p>
          <ul className="turns">
            {agents
              .filter((a) => a.installed && a.id !== current && a.kind !== "shell")
              .map((a) => (
                <li key={a.id}>
                  <span className="grow">{a.name}</span>
                  <button className="btn plain" disabled={busy} onClick={() => void to(a)}>
                    Switch
                  </button>
                </li>
              ))}
          </ul>
          {said && <p className="notice small">{said}</p>}
        </div>
      )}
    </span>
  );
}

/** A line of a unified diff, by its look: added, removed, a hunk or file header. */
function diffKind(line: string): string {
  if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ") || line.startsWith("index ")) return "ctx";
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "del";
  if (line.startsWith("@@")) return "gap";
  return "";
}

/** Each kept turn of the chat's agent (in a git repository): what it changed, its own diff, and putting the files back
 * as they were before it. Hidden where there are none (or no repository). */
function TurnsChip({ chatId }: { chatId: string }) {
  const [open, setOpen] = useState(false);
  const [turns, setTurns] = useState<ChatTurn[] | null>(null);
  const [diff, setDiff] = useState<{ n: number; text: string } | null>(null);
  const [said, setSaid] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    const look = () => void desktop.chatTurns(chatId).then(setTurns, () => setTurns(null));
    look();
    const timer = setInterval(look, open ? 3000 : 15000);
    return () => clearInterval(timer);
  }, [chatId, open]);
  useEffect(() => {
    if (!diff) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setDiff(null);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [diff]);
  if (!turns || turns.length === 0) return null;
  const time = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  async function restore(t: ChatTurn) {
    const sure = await ask(
      `Put the files back as they were before turn ${t.n}? Changes since (by any turn, or yours) are undone; this restore is kept as a turn, so you can undo it the same way. Let the agent finish its turn first.`,
      { title: "Marv", kind: "warning" },
    );
    if (!sure) return;
    setBusy(true);
    try {
      setSaid(await desktop.chatTurnRestore(chatId, t.n));
      setTurns(await desktop.chatTurns(chatId));
    } catch (e) {
      setSaid(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <span className="branch-chip">
      <button className="chip" aria-expanded={open} onClick={() => setOpen((o) => !o)} title="Each turn of this chat's agent that changed files: its diff, and going back to before it">
        {turns.length} {turns.length === 1 ? "turn" : "turns"}
      </button>
      {open && (
        <div className="branch-pop turns-pop" role="dialog" aria-label="This chat's turns">
          <ul className="turns">
            {[...turns].reverse().map((t) => (
              <li key={t.n}>
                <span className="grow" title={t.files.map(([k, p]) => `${k} ${p}`).join("\n")}>
                  <b>{t.label ?? `Turn ${t.n}`}</b>{" "}
                  <span className="muted small">
                    {time(t.endedMs)} · {t.files.length} {t.files.length === 1 ? "file" : "files"} <span className="add">+{t.added}</span>{" "}
                    <span className="del">−{t.removed}</span>
                  </span>
                </span>
                <button className="btn plain" onClick={() => void desktop.chatTurnDiff(chatId, t.n).then((text) => setDiff({ n: t.n, text }), (e) => setSaid(String(e)))}>
                  Diff
                </button>
                <button className="btn plain" disabled={busy} title={`The files back as they were before turn ${t.n}`} onClick={() => void restore(t)}>
                  Restore
                </button>
              </li>
            ))}
          </ul>
          {said && <p className="notice small">{said}</p>}
        </div>
      )}
      {diff && (
        <div className="modal" role="dialog" aria-label={`Turn ${diff.n}'s changes`} onClick={() => setDiff(null)}>
          <div className="modal-card turn-diff" onClick={(e) => e.stopPropagation()}>
            <div className="modal-head">
              <h2>Turn {diff.n}</h2>
              <button className="icon-btn" title="Close" onClick={() => setDiff(null)}>
                ×
              </button>
            </div>
            <pre className="code diff">
              {diff.text.split("\n").map((line, i) => (
                <span key={i} className={diffKind(line)}>
                  {line || " "}
                </span>
              ))}
            </pre>
          </div>
        </div>
      )}
    </span>
  );
}

/** The chat's permission mode (the same four for every agent): picked here, taken by its agent when its terminal next
 * starts, so a running one offers a restart. */
function PermissionChip({ chatId }: { chatId: string }) {
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<string | null>(null);
  const [said, setSaid] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  // Full access is offered only where Marv's sandbox can run (not on Windows yet).
  const [sandbox, setSandbox] = useState(true);
  useEffect(() => {
    desktop.info().then((i) => setSandbox(i.sandbox), () => {});
    desktop.chats().then((all) => setMode(all.find((c) => c.id === chatId)?.permissions ?? "own"), () => setMode("own"));
  }, [chatId]);
  useEffect(() => {
    if (open) void desktop.chatTerminalRunning(chatId).then(setRunning, () => setRunning(false));
  }, [open, chatId]);
  async function pick(id: string) {
    try {
      const held = await desktop.setChatPermissions(chatId, id);
      setMode(id);
      setSaid(held !== id ? `This folder holds it at ${permissionName(held)}: Marv never runs without asking in your home folder.` : null);
    } catch (e) {
      setSaid(String(e));
    }
  }
  if (mode === null) return null;
  return (
    <span className="branch-chip">
      <button className="chip" aria-expanded={open} onClick={() => setOpen((o) => !o)} title="How much this chat's agent may do without asking">
        {permissionName(mode)}
      </button>
      {open && (
        <div className="branch-pop" role="dialog" aria-label="This chat's permissions">
          {permissionModes(sandbox || mode === "full").map((m) => (
            <label key={m.id} className="perm-mode">
              <input type="radio" name={`perm-${chatId}`} checked={mode === m.id} onChange={() => void pick(m.id)} />
              <span>
                <b>{m.name}</b> <span className="muted small">{m.text}</span>
              </span>
            </label>
          ))}
          {said && <p className="notice small">{said}</p>}
          {running && (
            <div className="branch-actions">
              <span className="muted small grow">Its agent takes it when its terminal starts again.</span>
              <button className="btn" onClick={() => void desktop.chatTerminalClose(chatId).then(() => setRunning(false), () => {})}>
                Restart it
              </button>
            </div>
          )}
        </div>
      )}
    </span>
  );
}

function BranchChip({ chatId, onGone }: { chatId: string; onGone?: () => void }) {
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<WorktreeStatus | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!open) return;
    const look = () => void desktop.worktreeStatus(chatId).then(setStatus, () => setStatus(null));
    look();
    const timer = setInterval(look, 3000);
    return () => clearInterval(timer);
  }, [open, chatId]);
  const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;
  async function finish(kind: "merge" | "discard") {
    if (!status || busy) return;
    // Busy from the question on: a second click while it's open would ask (and merge) twice.
    setBusy(true);
    const sure = await ask(
      kind === "merge"
        ? `Merge ${status.branch} into ${status.into ?? "the folder's branch"}?${status.uncommitted > 0 ? ` Its ${plural(status.uncommitted, "uncommitted change")} are committed first.` : ""} Then its worktree, branch and this chat are removed.`
        : `Discard ${status.branch}? Its worktree, branch and this chat are removed, and its work with them${status.commits + status.uncommitted > 0 ? ` (${plural(status.commits, "commit")}, ${plural(status.uncommitted, "uncommitted change")})` : ""}.`,
      { title: "Marv", kind: "warning" },
    );
    if (!sure) return setBusy(false);
    try {
      const said = kind === "merge" ? await desktop.worktreeMerge(chatId) : await desktop.worktreeDiscard(chatId);
      setOpen(false);
      await say(said, { title: "Marv" });
      // The chat is gone: so are its terminals (the agent's and the Git tab's).
      forgetTerm(`chat:${chatId}`);
      forgetTerm(`git:${chatId}`);
      onGone?.();
    } catch (e) {
      await say(String(e), { title: "Marv", kind: "error" });
    } finally {
      setBusy(false);
    }
  }
  return (
    <span className="branch-chip">
      <button className="chip" aria-expanded={open} onClick={() => setOpen((o) => !o)} title="This chat works on its own branch, in its own worktree">
        ⎇ {status?.branch.replace(/^marv\//, "") ?? "branch"}
      </button>
      {open && (
        <div className="branch-pop" role="dialog" aria-label="This chat's branch">
          {!status ? (
            <p className="faint">Looking…</p>
          ) : (
            <>
              <p>
                <b>{status.branch}</b>
                {status.into && <span className="muted"> → {status.into}</span>}
              </p>
              <p className="muted small">
                {status.gone
                  ? "Its worktree's folder is gone."
                  : status.commits + status.uncommitted === 0
                    ? "Nothing new yet."
                    : `${plural(status.commits, "commit")}, ${plural(status.uncommitted, "uncommitted change")} · ${plural(status.files, "file")} changed`}
              </p>
              {status.folderChanges > 0 && (
                <p className="notice small">The folder has {plural(status.folderChanges, "uncommitted change")} of its own: commit or stash them to merge.</p>
              )}
              <div className="branch-actions">
                <button className="btn primary" disabled={busy || status.gone || !status.into || status.folderChanges > 0} onClick={() => void finish("merge")}>
                  Merge
                </button>
                <button className="btn danger" disabled={busy} onClick={() => void finish("discard")}>
                  Discard
                </button>
              </div>
            </>
          )}
        </div>
      )}
    </span>
  );
}
