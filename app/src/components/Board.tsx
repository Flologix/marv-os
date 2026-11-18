import { ask } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import { type Board as BoardData, desktop } from "../lib/backend";

function age(atMs: number) {
  const minutes = Math.floor((Date.now() - atMs) / 60_000);
  if (minutes < 1) return "now";
  return minutes < 60 ? `${minutes}m` : minutes < 1440 ? `${Math.floor(minutes / 60)}h` : `${Math.floor(minutes / 1440)}d`;
}

/**
 * A rule's team board (runtime-kernel §19): what the chats under it posted (claims, notes, done), who and when. Chats
 * read it with board_read; a post wakes nobody. You post as yourself and clear any entry; entries go after 24 hours.
 * Shown in Teams and in a chat's dock (kernel #53). `rules`: more than one for a team in worktrees (a board per pair of
 * its folders), shown as one; your post goes to each.
 */
export function Board({ rules, compact }: { rules: number[]; compact?: boolean }) {
  const [board, setBoard] = useState<BoardData | null>(null);
  const [kind, setKind] = useState("note");
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);

  const key = rules.join(",");
  useEffect(() => {
    setBoard(null);
    const load = () => void desktop.board(rules).then(setBoard, (e) => setError(String(e)));
    load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  }, [key]); // eslint-disable-line react-hooks/exhaustive-deps

  async function post() {
    if (!text.trim()) return;
    setError(null);
    try {
      setBoard(await desktop.boardPost(rules, kind, text));
      setText("");
    } catch (e) {
      setError(String(e));
    }
  }

  async function clear(entry?: [number, number][]) {
    if (entry === undefined && !(await ask("Clear this board? Every entry on it goes.", { title: "Marv", kind: "warning" }))) return;
    try {
      setBoard(await desktop.boardClear(rules, entry));
    } catch (e) {
      setError(String(e));
    }
  }

  const entries = board?.entries ?? [];
  return (
    <div className={`board${compact ? " compact" : ""}`}>
      {board && entries.length === 0 && <p className="faint board-empty">empty: chats post here what they're on (board_post)</p>}
      {entries.map((e) => (
        <div key={e.refs.join("|")} className="board-entry" title={e.paths.length ? `paths: ${e.paths.join(", ")}` : undefined}>
          <span className="board-meta">
            <span className={`board-kind ${e.kind}`}>{e.kind}</span>{" "}
            <span className="muted">
              {e.by ? e.by : <span className="board-you" title="You posted this, from the desktop">you</span>} · {age(e.atMs)}
            </span>
            <button className="icon-btn danger" title="Clear this entry" onClick={() => void clear(e.refs)}>
              ×
            </button>
          </span>
          <span className="board-text">{e.text}</span>
        </div>
      ))}
      <div className="board-post">
        <select value={kind} onChange={(e) => setKind(e.target.value)} aria-label="Kind">
          <option value="note">note</option>
          <option value="claim">claim</option>
          <option value="done">done</option>
        </select>
        <input value={text} maxLength={280} placeholder="one line for the chats under this rule" onChange={(e) => setText(e.target.value)} onKeyDown={(e) => e.key === "Enter" && void post()} />
        <button className="btn primary" disabled={!text.trim()} onClick={() => void post()}>
          Post
        </button>
        {entries.length > 0 && !compact && (
          <button className="btn plain" onClick={() => void clear()}>
            Clear all
          </button>
        )}
      </div>
      {board?.problem && <p className="notice error">This board isn't used: {board.problem}</p>}
      {error && <p className="notice error">{error}</p>}
    </div>
  );
}
