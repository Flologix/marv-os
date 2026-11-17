import { ask } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { appFor } from "../lib/apps";
import { type AppInfo, type AppStatus, desktop } from "../lib/backend";
import type { ServerStatus } from "../lib/chat";
import { AccountGate } from "./AccountGate";
import { Permissions } from "./Activity";
import { useChatDevices, DeviceSwitches } from "./Devices";
import { Gallery } from "./Gallery";
import { Servers } from "./Servers";

/**
 * Kernel #53, Apps: what chats can reach. Your connected apps first (signed in or not, on or off in this folder),
 * then apps to connect (OAuth sign-in in the browser, no chat needed), the folder's own servers, the open chat's
 * devices, and every permission (show and revoke).
 */
export function Apps({
  folder,
  folderName,
  servers,
  chatOpen,
  chatId,
  chatTitle,
  mcpOff,
  onToggleServer,
  onServerAction,
  onServersChanged,
  onBack,
  info,
  onInfo,
}: {
  folder: string;
  folderName: string;
  /** The open chat's servers' status (status, login and trust need an open chat: the servers run there). */
  servers: ServerStatus[];
  chatOpen: boolean;
  chatId: string | null;
  chatTitle?: string;
  mcpOff: string[];
  onToggleServer: (name: string, on: boolean) => void;
  onServerAction: (action: string, name?: string) => void;
  onServersChanged: () => void;
  /** Back to Settings, where Apps is reached from. */
  onBack?: () => void;
  /** Adding an app needs a (free) Marv account. */
  info: AppInfo;
  onInfo: (info: AppInfo) => void;
}) {
  const [mine, setMine] = useState<AppStatus[] | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const gallery = useRef<HTMLElement>(null);
  const devices = useChatDevices(chatId);
  const refresh = useCallback(() => desktop.appsStatus().then(setMine, (e) => setError(String(e))), []);
  useEffect(() => {
    void refresh();
    // A sign-in or sign-out in the terminal (`/mcp login`) shows once the window is back in front.
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [refresh]);

  async function run(name: string, work: () => Promise<unknown>, done?: string) {
    setError(null);
    setNote(null);
    setBusy(name);
    try {
      await work();
      if (done) setNote(done);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      void refresh();
    }
  }

  const signIn = (name: string) =>
    run(name, async () => {
      const tools = await desktop.appSignIn(name);
      setNote(`${title(name)} is signed in · ${tools} tool${tools === 1 ? "" : "s"}. New chats can use it.`);
    });
  const signOut = (name: string) => run(name, () => desktop.appSignOut(name), `${title(name)} is signed out.`);
  async function remove(name: string) {
    if (!(await ask(`Remove ${title(name)}? Chats in every folder stop reaching it. Its sign-in is forgotten too.`, { title: "Marv", kind: "warning" })))
      return;
    await run(name, async () => {
      const app = mine?.find((m) => m.name === name);
      if (app?.signedIn) await desktop.appSignOut(name).catch(() => {});
      await desktop.mcpRemove(name);
      onServersChanged();
    });
  }

  const connected = mine ?? [];
  return (
    <main className="place apps" aria-label="Apps">
      <div className="place-col">
        <header className="apps-head">
          {onBack && (
            <button className="link" onClick={onBack}>
              ‹ Settings
            </button>
          )}
          <h1 className="place-title">Apps</h1>
          <p className="muted">
            Sign Marv in to the tools you work in. Each app opens its own sign-in page in your browser; Marv never sees your password, and every chat in
            every folder can use what you connect. You approve what chats do with them in Permissions below.
          </p>
        </header>

        <section className="settings-section" aria-labelledby="apps-connected-h">
          <header>
            <h2 id="apps-connected-h">Connected</h2>
            <p>Switch an app off for chats in {folderName}; new chats pick up a change.</p>
          </header>
          {mine === null ? (
            <p className="faint small">Loading…</p>
          ) : connected.length === 0 ? (
            <div className="settings-list">
              <div className="settings-item">
                <span className="grow muted small">Nothing yet. Connect your mail, calendar or Slack below.</span>
                <button className="btn" onClick={() => gallery.current?.scrollIntoView({ behavior: "smooth", block: "start" })}>
                  Browse apps
                </button>
              </div>
            </div>
          ) : (
            <div className="settings-list conns">
              {connected.map((m) => {
                const on = !mcpOff.includes(m.name);
                const app = appFor(m.name);
                const needs = m.kind === "oauth" && !m.signedIn;
                return (
                  <div key={m.name} className={`conn app-row${needs ? " needs" : ""}`}>
                    <div className="settings-item">
                      <span className="app-mark">{app?.mark ?? m.name.slice(0, 2).toUpperCase()}</span>
                      <span className="grow conn-text">
                        <span className="conn-title">
                          <b>{title(m.name)}</b>
                          <StatePill app={m} />
                        </span>
                        <span className="muted small">{detail(m)}</span>
                      </span>
                      <span className="conn-actions">
                        {busy === m.name ? (
                          <>
                            <span className="spinner" aria-label="Working" />
                            {needs && (
                              <button className="btn plain" onClick={() => void desktop.appCancelSignIn()}>
                                Cancel
                              </button>
                            )}
                          </>
                        ) : (
                          <>
                            {needs && (
                              <button className="btn primary" onClick={() => void signIn(m.name)}>
                                Sign in
                              </button>
                            )}
                            {m.kind === "oauth" && m.signedIn && (
                              <button className="btn plain" onClick={() => void signOut(m.name)}>
                                Sign out
                              </button>
                            )}
                            <button className="link danger small" onClick={() => void remove(m.name)}>
                              Remove
                            </button>
                          </>
                        )}
                        <button
                          type="button"
                          role="switch"
                          aria-checked={on}
                          aria-label={`${title(m.name)} in ${folderName}`}
                          title={on ? `On for chats in ${folderName}: switch off` : `Off for chats in ${folderName}: switch on`}
                          className={`toggle small${on ? " on" : ""}`}
                          onClick={() => onToggleServer(m.name, !on)}
                        />
                      </span>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
          {note && <p className="notice">{note}</p>}
          {error && <p className="notice error">{error}</p>}
        </section>

        <section ref={gallery} className="settings-section" aria-labelledby="apps-add-h">
          <header>
            <h2 id="apps-add-h">Add an app</h2>
            <p>Mail, calendar, chat and docs your business runs on. Some need a one-time setup by your Google, Microsoft or Slack admin.</p>
          </header>
          <AccountGate info={info} onInfo={onInfo} feature="Apps" pitch="Sign Marv in to your mail, calendar, Slack, Notion and GitHub, for every chat to use.">
            <Gallery mine={connected} onConnected={(n) => (setNote(n ?? null), setError(null), void refresh())} />
          </AccountGate>
        </section>

        <section className="settings-section" aria-labelledby="apps-folder-h">
          <header>
            <h2 id="apps-folder-h">Other servers</h2>
            <p>Any MCP server by package, URL or repo, and the ones {folderName}'s own .mcp.json brings (trusted per folder, from an open chat).</p>
          </header>
          <Servers
            folder={folder}
            servers={servers}
            chatOpen={chatOpen}
            mcpOff={mcpOff}
            onToggle={onToggleServer}
            onAction={onServerAction}
            onChanged={onServersChanged}
            onAdded={() => void refresh()}
            folderOnly
          />
        </section>

        {devices && (
          <section className="settings-section" aria-labelledby="apps-devices-h">
            <header>
              <h2 id="apps-devices-h">Devices</h2>
              <p>What {chatTitle || "this chat"} may reach right now.</p>
            </header>
            <div className="apps-devices">
              <DeviceSwitches devices={devices} />
            </div>
          </section>
        )}

        <section className="settings-section" aria-labelledby="apps-perms-h">
          <header>
            <h2 id="apps-perms-h">Permissions</h2>
            <p>Everything you've allowed, and where. Revoke any of it.</p>
          </header>
          <Permissions />
        </section>
      </div>
    </main>
  );
}

const title = (name: string) => appFor(name)?.name ?? name;

function detail(m: AppStatus): string {
  const blurb = appFor(m.name)?.blurb;
  if (m.kind === "oauth" && m.signedIn) return m.host ? `Signed in at ${m.host}` : blurb ?? "Signed in";
  if (m.kind === "oauth") return "Sign in to let chats use it.";
  if (m.kind === "key") return blurb ?? "Uses the key from its setup.";
  return blurb ?? "Runs on this computer.";
}

function StatePill({ app }: { app: AppStatus }) {
  if (app.kind === "oauth") return app.signedIn ? <span className="pill ok">Signed in</span> : <span className="pill warn">Needs sign-in</span>;
  if (app.kind === "key") return <span className="pill ready">Key</span>;
  return <span className="pill ready">On this computer</span>;
}
