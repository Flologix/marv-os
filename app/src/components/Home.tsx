import { type Away, type ChatRecord, type LiveChat } from "../lib/backend";
import { harnessTitle } from "../lib/harness";
import { type Need, type Place, chatTone, greeting } from "../lib/places";
import { awayText } from "./Activity";
import { ToneMarv } from "./Martian";

/**
 * Kernel #53, Home: what needs you (approvals waiting in any chat, grants paused at their cap or held, budgets reached),
 * each with Open, the Home chats (a chat for anything on this computer, in the home folder), and your recent chats in
 * every folder, with "while you were away" in one line (runtime-kernel §20.3: the kernel's counts, never a message's
 * words). The kernel's own log stays out of it: names and hops were the machine's view, not the person's.
 */
export function Home({
  needs,
  running,
  todayUsd,
  capUsd,
  away,
  onOpenChat,
  onPlace,
  homeChats,
  recent,
  live,
  onNewHomeChat,
}: {
  needs: Need[];
  running: number;
  todayUsd: number | null;
  /** The everything budget, when set. */
  capUsd: number | null;
  away: Away | null;
  onOpenChat: (id: string) => void;
  onPlace: (place: Place) => void;
  /** The chats in the home folder, any order. */
  homeChats: ChatRecord[];
  /** The chats in folders, any order: the newest are shown. */
  recent: ChatRecord[];
  live: LiveChat[];
  /** A new Home chat (its draft, in Folders). */
  onNewHomeChat: () => void;
}) {
  const summary = [`${running} running`, `${needs.length} need${needs.length === 1 ? "s" : ""} you`];
  if (todayUsd !== null) summary.push(`today $${todayUsd.toFixed(2)}${capUsd !== null ? ` of $${capUsd.toFixed(2)}` : ""}`);
  const folderOf = (c: ChatRecord) => c.folder.split(/[\\/]/).filter(Boolean).pop() ?? c.folder;
  const shown = [...recent].sort((a, b) => b.created - a.created).slice(0, 8);

  return (
    <main className="place home" aria-label="Home">
      <div className="place-col">
        <div>
          <h1 className="place-title">{greeting(new Date().getHours())}</h1>
          <span className="place-meta">{summary.join(" · ")}</span>
        </div>
        <section className="home-ask">
          <button className="home-ask-box" onClick={onNewHomeChat} title="A chat in your home folder: anything on this computer">
            <span className="muted">Ask Marv anything on this computer…</span>
            <span className="btn primary">New chat</span>
          </button>
          {homeChats.length > 0 && (
            <div className="home-recent">
              {[...homeChats]
                .sort((a, b) => b.created - a.created)
                .slice(0, 5)
                .map((c) => {
                  const { tone, word } = chatTone(live.find((l) => l.id === c.id));
                  return (
                    <button key={c.id} className="chat-row" onClick={() => onOpenChat(c.id)} title={word}>
                      <ToneMarv tone={tone} />
                      <span className="title">{c.title}</span>
                    </button>
                  );
                })}
            </div>
          )}
        </section>
        <section>
          <h2 className="place-h needs">Needs you</h2>
          {needs.length === 0 && <p className="faint place-empty">Nothing needs you.</p>}
          {needs.map((n) => (
            <div key={n.key} className="place-row">
              <span className="grow">
                <b>{n.title}</b> <span className="muted">{n.what}</span>
              </span>
              <button className="btn" onClick={() => (n.chat ? onOpenChat(n.chat) : n.place && onPlace(n.place))}>
                Open
              </button>
            </div>
          ))}
        </section>
        <section>
          <h2 className="place-h">{away ? "While you were away" : "Recent chats"}</h2>
          {away && <p className="away-sum">{awayText(away)}</p>}
          {shown.length === 0 && <p className="faint place-empty">No chats in your folders yet.</p>}
          <div className="home-recent">
            {shown.map((c) => {
              const { tone, word } = chatTone(live.find((l) => l.id === c.id));
              return (
                <button key={c.id} className="chat-row" onClick={() => onOpenChat(c.id)} title={word}>
                  <ToneMarv tone={tone} />
                  <span className="title">{c.title}</span>
                  <span className="faint small">
                    {folderOf(c)} · {harnessTitle(c.harness)}
                  </span>
                </button>
              );
            })}
          </div>
        </section>
      </div>
    </main>
  );
}
