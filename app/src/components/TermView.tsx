import { useEffect, useRef } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { desktop } from "../lib/backend";

/** Windows keys: ctrl+v pastes (as in Windows Terminal), and a picture is pasted with Claude Code's Alt+V. */
const WINDOWS = navigator.userAgent.includes("Windows");

/**
 * Flexoki's dark colors, for every terminal whatever the page's theme: terminal agents draw for a dark terminal (Marv's
 * status bar is a black bar whose text is the terminal's default color, so on a light terminal it read as a black
 * streak with nothing in it), as editors with a light theme still show their terminal dark.
 */
const THEME = {
  background: "#100F0F",
  foreground: "#CECDC3",
  cursor: "#CECDC3",
  selectionBackground: "#403E3C",
  black: "#100F0F",
  red: "#D14D41",
  green: "#879A39",
  yellow: "#D0A215",
  blue: "#4385BE",
  magenta: "#8B7EC8",
  cyan: "#3AA99F",
  white: "#CECDC3",
  brightBlack: "#575653",
  brightRed: "#D14D41",
  brightGreen: "#879A39",
  brightYellow: "#DA702C",
  brightBlue: "#4385BE",
  brightMagenta: "#CE5D97",
  brightCyan: "#3AA99F",
  brightWhite: "#FFFCF0",
};

const fromBase64 = (b64: string) => Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));

export type TermMessage = { data?: string; exit?: boolean };
type TermState = "running" | "ended" | { error: string };

/**
 * A terminal kept for the page's life, shown or not: its output keeps arriving while another chat shows, so coming
 * back puts it back exactly as it is (asking the agent to draw again on a fresh one left some blank or half drawn).
 */
interface Kept {
  term: XTerm;
  fit: FitAddon;
  /** Where xterm draws: moved into whichever view shows it. */
  wrap: HTMLDivElement;
  id: number | null;
  state: TermState | null;
  /** The showing view's state callback (none while hidden). */
  onState: ((s: TermState) => void) | null;
}

const kept = new Map<string, Kept>();
// Development builds only: the kept terminals, for the user-flow harness's measurements (marv-os-sim).
if (import.meta.env.DEV) (window as unknown as { __marvTerms: Map<string, Kept> }).__marvTerms = kept;

/** The terminal may take the keys: nothing else (an input, a menu's field) is being typed in. */
const freeToFocus = () => !document.activeElement?.closest("input, textarea:not(.xterm-helper-textarea), select, [contenteditable]");

/** Ends a kept terminal's view (it's started afresh: its agent ended, or the person asked for a fresh one). */
export function forgetTerm(key: string) {
  const k = kept.get(key);
  if (!k) return;
  k.term.dispose();
  kept.delete(key);
}

/**
 * A terminal the app runs (a chat's agent, the Git tab's lazygit), drawn with xterm.js and kept under `cacheKey` for
 * the page's life. `open` starts it, or attaches to the running one, at the view's size, and gives its id. A new
 * `run` (with the key forgotten) starts it afresh.
 */
export function TermView({
  cacheKey,
  open,
  run,
  onState,
  onId,
  scrollback = 0,
}: {
  cacheKey: string;
  open: (cols: number, rows: number, onData: (m: TermMessage) => void) => Promise<number>;
  run: unknown;
  onState: (state: TermState) => void;
  onId?: (id: number | null) => void;
  scrollback?: number;
}) {
  const host = useRef<HTMLDivElement>(null);
  const calls = useRef({ open, onState, onId });
  calls.current = { open, onState, onId };

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    let k = kept.get(cacheKey);
    if (!k) {
      const wrap = document.createElement("div");
      wrap.className = "term-wrap";
      const term = new XTerm({
        fontFamily: getComputedStyle(document.documentElement).getPropertyValue("--font-mono").trim() || "monospace",
        fontSize: 12,
        theme: THEME,
        scrollback,
        cursorBlink: false,
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      const fresh: Kept = { term, fit, wrap, id: null, state: null, onState: null };
      kept.set(cacheKey, fresh);
      k = fresh;
      el.appendChild(wrap);
      term.open(wrap);
      if (el.clientWidth > 0) fit.fit();
      // Whatever changes the terminal's size (the view's, the font's cells once the font has loaded), the agent's
      // terminal gets it: it drew at the size measured before the font came, a quarter narrower than the pane.
      term.onResize(({ cols, rows }) => fresh.id !== null && void desktop.gitTerminalResize(fresh.id, cols, rows).catch(() => {}));
      void document.fonts?.ready.then(() => wrap.isConnected && fit.fit());
      const tell = (s: TermState) => {
        fresh.state = s;
        fresh.onState?.(s);
      };
      calls.current
        .open(term.cols, term.rows, (m) => {
          if (m.data) term.write(fromBase64(m.data));
          if (m.exit) tell("ended");
        })
        .then(
          (id) => {
            fresh.id = id;
            if (fresh.onState) calls.current.onId?.(id);
            tell(fresh.state === "ended" ? "ended" : "running");
            // Measured again now it's running: the size it was started at may be stale already.
            if (wrap.isConnected && el.clientWidth > 0) fit.fit();
            void desktop.gitTerminalResize(id, term.cols, term.rows).catch(() => {});
            if (freeToFocus()) term.focus();
          },
          (e) => tell({ error: String(e) }),
        );
      // Drawn by the GPU, set up once the agent was asked to start (before, the agent waited for it: a chat's
      // terminal was asked for a quarter second after its click, the first one two seconds): the DOM renderer's cells
      // made selecting text lag in the desktop's web view. Back to it if the
      // GPU's context is lost or can't be had.
      try {
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => webgl.dispose());
        term.loadAddon(webgl);
      } catch {
        // The DOM renderer, as before.
      }
      // Copying. An agent's TUI copies with OSC 52 (`ESC ] 52 ; c ; <base64> BEL`): Claude Code said "copied" and the
      // clipboard kept what it had, as xterm leaves the sequence to the app. A request to read the clipboard ("?") is
      // refused: what's on it is the person's, not the agent's.
      term.parser.registerOscHandler(52, (data) => {
        const b64 = data.slice(data.indexOf(";") + 1);
        if (b64 && b64 !== "?") {
          try {
            void desktop.clipboardWrite(new TextDecoder().decode(fromBase64(b64))).catch(() => {});
          } catch {
            // Not base64: nothing to copy.
          }
        }
        return true;
      });
      // Copying and pasting as the person's own terminal does (on Windows, as Windows Terminal does):
      // - selecting copies (shift+drag where the agent takes the mouse): selecting and nothing reaching the clipboard
      //   was the first thing a person on Windows tried, and reported;
      // - ctrl+c copies while there's a selection (and clears it), else it's the agent's, as ever; ctrl+shift+c too;
      // - right-click copies the selection, or pastes when there's none;
      // - ctrl+shift+v pastes, and on Windows ctrl+v too ([`pasteClipboard`]).
      const copy = () => {
        if (term.hasSelection()) void desktop.clipboardWrite(term.getSelection()).catch(() => {});
      };
      // A terminal pastes text only: with a picture on the clipboard (a screenshot) the paste was empty and nothing
      // happened. Claude Code reads the clipboard's picture itself on its own key, Alt+V on Windows, so that's sent.
      const pasteClipboard = async () => {
        const text = await desktop.clipboardRead().catch(() => "");
        if (text) term.paste(text);
        else if (WINDOWS && fresh.id !== null && (await desktop.clipboardHasImage().catch(() => false)))
          void desktop.gitTerminalWrite(fresh.id, "\x1bv").catch(() => {});
      };
      let copyTimer: ReturnType<typeof setTimeout> | undefined;
      term.onSelectionChange(() => {
        clearTimeout(copyTimer);
        // Once the drag ends, not on every cell it crosses.
        copyTimer = setTimeout(copy, 150);
      });
      wrap.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        if (term.hasSelection()) {
          copy();
          term.clearSelection();
        } else void pasteClipboard();
      });
      term.attachCustomKeyEventHandler((e) => {
        if (e.type !== "keydown" || !e.ctrlKey || e.altKey || e.metaKey) return true;
        const key = e.key.toLowerCase();
        if (key === "c" && term.hasSelection()) {
          copy();
          if (!e.shiftKey) term.clearSelection();
          return false;
        }
        if (key === "v" && (e.shiftKey || WINDOWS)) {
          // Not the web view's paste as well: the text would go in twice.
          e.preventDefault();
          void pasteClipboard();
          return false;
        }
        return true;
      });
      term.onData((data) => fresh.id !== null && void desktop.gitTerminalWrite(fresh.id, data).catch(() => {}));
    } else {
      el.appendChild(k.wrap);
      if (el.clientWidth > 0) k.fit.fit();
      if (k.id !== null) void desktop.gitTerminalResize(k.id, k.term.cols, k.term.rows).catch(() => {});
      if (freeToFocus()) k.term.focus();
    }
    const shown = k;
    shown.onState = (s) => calls.current.onState(s);
    if (shown.state) calls.current.onState(shown.state);
    calls.current.onId?.(shown.id);
    // Drawn again from its own buffer when it shows again (back from Activity, another place or chat): hidden, the
    // GPU canvas lost its picture, and xterm only draws again for new output, so an idle agent's terminal stayed
    // black until the agent drew something.
    const redraw = () => shown.term.refresh(0, shown.term.rows - 1);
    redraw();
    // The view's size: `fit` changes the terminal's, and `onResize` tells the agent. A size from nothing is the view
    // showing again.
    let wasHidden = el.clientWidth === 0;
    // Fitted once the size settles: dragging a pane edge fires it per frame, and each fit reflows the whole buffer.
    let fitTimer: ReturnType<typeof setTimeout> | undefined;
    const resized = new ResizeObserver(() => {
      const hidden = el.clientWidth === 0;
      const back = wasHidden;
      wasHidden = hidden;
      clearTimeout(fitTimer);
      if (hidden) return;
      fitTimer = setTimeout(() => {
        if (el.clientWidth === 0 || !shown.wrap.isConnected) return;
        shown.fit.fit();
        if (back) redraw();
      }, 80);
    });
    // Back to the window (from another app): the keys go to the terminal it shows, not to whatever had focus.
    const refocus = () => shown.wrap.isConnected && freeToFocus() && shown.term.focus();
    window.addEventListener("focus", refocus);
    resized.observe(el);
    return () => {
      resized.disconnect();
      clearTimeout(fitTimer);
      window.removeEventListener("focus", refocus);
      shown.onState = null;
      // Kept: out of this view, still running and receiving.
      if (shown.wrap.parentElement === el) el.removeChild(shown.wrap);
    };
  }, [cacheKey, run, scrollback]);

  return <div className="git-term" ref={host} />;
}
