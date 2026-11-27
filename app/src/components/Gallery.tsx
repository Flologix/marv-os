import { useMemo, useState } from "react";
import {
  APPS,
  type App,
  CATEGORIES,
  type Category,
  type KeysConnect,
  MARV_CLIENTS,
  type OAuthConnect,
  PROVIDERS,
  clientsFor,
  fillIn,
  redirectUri,
} from "../lib/apps";
import { type AppStatus, type Lookup, desktop } from "../lib/backend";

/**
 * Apps to connect, by kind, with a search. Connect opens the app's panel in place: an OAuth app signs in through the
 * browser (after a one-time organization client where the provider needs one); a keys app shows exactly what will be
 * added first.
 */
export function Gallery({ mine, onConnected }: { mine: AppStatus[]; onConnected: (note?: string) => void }) {
  const [open, setOpen] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<Category | null>(null);
  const installed = new Set(mine.map((m) => m.name));
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    return APPS.filter((a) => (!category || a.category === category) && (!q || `${a.name} ${a.blurb} ${a.category}`.toLowerCase().includes(q)));
  }, [query, category]);

  return (
    <div className="gallery-wrap">
      <div className="gallery-bar">
        <input className="gallery-search" type="search" placeholder="Search apps" value={query} spellCheck={false} onChange={(e) => setQuery(e.target.value)} />
        <div className="chips" role="tablist" aria-label="Kinds of app">
          <button role="tab" aria-selected={category === null} className={`chip${category === null ? " on" : ""}`} onClick={() => setCategory(null)}>
            All
          </button>
          {CATEGORIES.map((c) => (
            <button key={c} role="tab" aria-selected={category === c} className={`chip${category === c ? " on" : ""}`} onClick={() => setCategory(c)}>
              {c}
            </button>
          ))}
        </div>
      </div>
      {shown.length === 0 && <p className="faint small">No app matches. Add any MCP server under Other servers below.</p>}
      <div className="gallery">
        {shown.map((app) =>
          open === app.id ? (
            <Connect
              key={app.id}
              app={app}
              mine={mine}
              onDone={(note) => {
                setOpen(null);
                onConnected(note);
              }}
              onCancel={() => setOpen(null)}
            />
          ) : (
            <div key={app.id} className="tile">
              <span className="app-mark">{app.mark}</span>
              <div className="tile-text">
                <b>{app.name}</b>
                <span>{app.blurb}</span>
              </div>
              {installed.has(app.id) ? (
                <span className="pill ok">Connected</span>
              ) : (
                <button className="btn" onClick={() => setOpen(app.id)}>
                  Connect
                </button>
              )}
            </div>
          ),
        )}
      </div>
    </div>
  );
}

function Connect({ app, mine, onDone, onCancel }: { app: App; mine: AppStatus[]; onDone: (note?: string) => void; onCancel: () => void }) {
  const [alt, setAlt] = useState(false);
  const connect = alt && app.alt ? app.alt.connect : app.connect;
  return (
    <div className="tile open">
      <div className="tile-head">
        <span className="app-mark">{app.mark}</span>
        <div className="tile-text">
          <b>Connect {app.name}</b>
          <span>{app.blurb}</span>
        </div>
      </div>
      {app.note && <p className="help">{app.note}</p>}
      {connect.kind === "oauth" ? (
        <OAuthPanel app={app} connect={connect} mine={mine} onDone={onDone} onCancel={onCancel} />
      ) : (
        <KeysPanel app={app} connect={connect} onDone={onDone} onCancel={onCancel} />
      )}
      {app.alt && (
        <button className="link small" onClick={() => setAlt(!alt)}>
          {alt ? `Sign in with ${app.name} instead` : app.alt.label}
        </button>
      )}
    </div>
  );
}

type Step = "setup" | "waiting" | "failed";

function OAuthPanel({
  app,
  connect,
  mine,
  onDone,
  onCancel,
}: {
  app: App;
  connect: OAuthConnect;
  mine: AppStatus[];
  onDone: (note?: string) => void;
  onCancel: () => void;
}) {
  const provider = connect.provider ? PROVIDERS[connect.provider] : null;
  const builtIn = connect.provider ? MARV_CLIENTS[connect.provider] : undefined;
  const reusable = connect.provider && !builtIn ? clientsFor(connect.provider, mine) : [];
  const [reuse, setReuse] = useState<string>(reusable[0] ?? "");
  const [values, setValues] = useState<Record<string, string>>(() => Object.fromEntries((connect.fields ?? []).map((f) => [f.name, f.value ?? ""])));
  const [clientId, setClientId] = useState("");
  const [clientSecret, setClientSecret] = useState("");
  const [step, setStep] = useState<Step>("setup");
  const [added, setAdded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const needsClient = !!provider && !builtIn && !reuse;
  const ready =
    (connect.fields ?? []).every((f) => values[f.name]?.trim()) && (!needsClient || (clientId.trim() && (!provider!.secret || clientSecret.trim())));
  const who = provider?.name ?? app.name;

  async function signIn() {
    setError(null);
    setStep("waiting");
    try {
      if (!added) {
        const client = builtIn ?? (needsClient ? { clientId: clientId.trim(), clientSecret: clientSecret.trim() || undefined } : undefined);
        await desktop.appConnect(
          app.id,
          fillIn(connect.url, values),
          {
            ...client,
            scope: connect.scope ? fillIn(connect.scope, values) : undefined,
            callbackPort: provider?.callbackPort,
          },
          reuse || undefined,
        );
        setAdded(true);
      }
      const tools = await desktop.appSignIn(app.id);
      onDone(`${app.name} is connected · ${tools} tool${tools === 1 ? "" : "s"}. New chats can use it.`);
    } catch (e) {
      setError(String(e));
      setStep("failed");
    }
  }

  async function cancel() {
    if (step === "waiting") await desktop.appCancelSignIn();
    // Added but never signed in: take it back out, so the gallery offers it again.
    if (added) await desktop.mcpRemove(app.id).catch(() => {});
    onCancel();
  }

  if (step === "waiting")
    return (
      <div className="oauth-wait">
        <span className="spinner" aria-hidden />
        <span className="grow">
          <b>Finish signing in in your browser.</b>
          <span className="muted small"> {who}'s page opened there; come back once you've allowed Marv.</span>
        </span>
        <button className="btn plain" onClick={() => void cancel()}>
          Cancel
        </button>
      </div>
    );

  return (
    <>
      {(connect.fields ?? []).map((f) => (
        <label key={f.name} className="field">
          <span>{f.label}</span>
          <input
            className="mono"
            type={f.secret ? "password" : "text"}
            placeholder={f.placeholder}
            value={values[f.name]}
            spellCheck={false}
            disabled={added}
            onChange={(e) => setValues((v) => ({ ...v, [f.name]: e.target.value }))}
          />
          {f.help && <span className="faint small">{f.help}</span>}
        </label>
      ))}

      {provider && !builtIn && reusable.length > 0 && !added && (
        <label className="field">
          <span>{provider.name} app</span>
          <select value={reuse} onChange={(e) => setReuse(e.target.value)}>
            {reusable.map((r) => (
              <option key={r} value={r}>
                The same one as {r}
              </option>
            ))}
            <option value="">Another {provider.name} app…</option>
          </select>
        </label>
      )}

      {needsClient && !added && (
        <div className="org-client">
          <p className="small">
            <b>One-time setup by your {provider!.name} admin.</b> {provider!.steps}{" "}
            <button className="link" onClick={() => void desktop.openUrl(provider!.console.url)}>
              {provider!.console.label} ↗
            </button>
            {provider!.guide && (
              <>
                {" · "}
                <button className="link" onClick={() => void desktop.openUrl(provider!.guide!)}>
                  Setup guide ↗
                </button>
              </>
            )}
          </p>
          <div className="field">
            <span>Redirect URI</span>
            <span className="copy-row">
              <code className="mono">{redirectUri(connect.provider!)}</code>
              <button
                className="btn plain small"
                onClick={() => void navigator.clipboard.writeText(redirectUri(connect.provider!)).then(() => setCopied(true))}
              >
                {copied ? "Copied" : "Copy"}
              </button>
            </span>
          </div>
          <label className="field">
            <span>Client ID</span>
            <input className="mono" value={clientId} spellCheck={false} onChange={(e) => setClientId(e.target.value)} />
          </label>
          {provider!.secret && (
            <label className="field">
              <span>Client secret</span>
              <input className="mono" type="password" value={clientSecret} spellCheck={false} onChange={(e) => setClientSecret(e.target.value)} />
            </label>
          )}
          <p className="faint small">Kept in ~/.marv/mcp.json on this computer (yours only), and reused for your other {provider!.name} apps.</p>
        </div>
      )}

      {error && <p className="notice error">{error}</p>}
      <div className="add-buttons">
        <button className="btn primary" disabled={!ready} onClick={() => void signIn()}>
          {step === "failed" ? "Try again" : `Sign in with ${who}`}
        </button>
        <button className="btn plain" onClick={() => void cancel()}>
          Cancel
        </button>
      </div>
      {step === "setup" && (
        <p className="faint small">Your browser opens {who}'s own sign-in page. Marv never sees your password, and you can sign out here any time.</p>
      )}
    </>
  );
}

function KeysPanel({ app, connect, onDone, onCancel }: { app: App; connect: KeysConnect; onDone: (note?: string) => void; onCancel: () => void }) {
  const [values, setValues] = useState<Record<string, string>>(() => Object.fromEntries(connect.fields.map((f) => [f.name, f.value ?? ""])));
  const [review, setReview] = useState<Extract<Lookup, { name: string }> | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ready = connect.fields.every((f) => values[f.name]?.trim());

  async function look() {
    setError(null);
    setWorking(true);
    try {
      const found = await desktop.mcpLookup(connect.source, app.id, connect.headers ? [] : connect.fields.map((f) => f.name), connect.headers);
      if ("choices" in found) setError("That source lists several servers; add it under Other servers.");
      else setReview(found);
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }

  async function add() {
    setError(null);
    setWorking(true);
    try {
      const filled = Object.fromEntries(Object.entries(values).map(([k, v]) => [k, v.trim()]));
      await desktop.mcpInstall(review!.name, filled);
      onDone(`${app.name} is added. New chats can use it.`);
    } catch (e) {
      setError(String(e));
      setWorking(false);
    }
  }

  return (
    <>
      {connect.setup && (
        <p className="help">
          {connect.setup.text}{" "}
          <button className="link" onClick={() => void desktop.openUrl(connect.setup!.url)}>
            Open ↗
          </button>
        </p>
      )}
      {connect.fields.map((f) => (
        <label key={f.name} className="field">
          <span>{f.label}</span>
          <input
            className="mono"
            type={f.secret ? "password" : "text"}
            placeholder={f.placeholder}
            value={values[f.name]}
            spellCheck={false}
            disabled={review !== null}
            onChange={(e) => setValues((v) => ({ ...v, [f.name]: e.target.value }))}
          />
        </label>
      ))}
      {review && (
        <div className="lookup">
          <b>{review.title}</b>
          {review.command && <pre className="code">$ {review.command}</pre>}
          <pre className="code">{review.text}</pre>
          {review.warning && <p className="warning">{review.warning}</p>}
          <p className="help">
            Added to ~/.marv/mcp.json (yours only{connect.fields.some((f) => f.secret) ? ", with the keys above" : ""}). Every folder gets it; switch it
            off in any folder.
          </p>
        </div>
      )}
      <div className="add-buttons">
        {review ? (
          <button className="btn primary" disabled={working} onClick={() => void add()}>
            {working ? "Adding…" : `Add ${app.name}`}
          </button>
        ) : (
          <button className="btn primary" disabled={!ready || working} onClick={() => void look()}>
            {working ? "Checking…" : "Continue"}
          </button>
        )}
        <button className="btn plain" onClick={onCancel}>
          Cancel
        </button>
      </div>
      {error && <p className="notice error">{error}</p>}
    </>
  );
}
