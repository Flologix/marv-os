import { useEffect, useState } from "react";
import { type AppInfo, desktop } from "../lib/backend";
import { PRICING_URL, type PlanStatus, isFree, planView } from "../lib/plan";

/**
 * Signing in to a Marv account (free, or the plan it has): Google or GitHub in the browser, or an email and the 6-digit
 * code the server emails. Every call goes from the Rust side (`plan_login_*`); the server's words show as plain text.
 */
export function MarvPlanSignIn({ onSignedIn, onCancel }: { onSignedIn: (status: PlanStatus) => void; onCancel?: () => void }) {
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [sent, setSent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Waiting for the browser: Google's or GitHub's page is open there. */
  const [browser, setBrowser] = useState<"google" | "github" | null>(null);
  /** Consent to product emails: the person's own tick, never on by default. */
  const [updates, setUpdates] = useState(false);

  async function run(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const send = () => run(async () => (await desktop.planLoginStart(email), setSent(true)));
  const verify = () => run(async () => onSignedIn(await desktop.planLoginVerify(email, code, updates)));
  const withBrowser = (provider: "google" | "github") =>
    run(async () => {
      setBrowser(provider);
      try {
        onSignedIn(await desktop.planLoginWith(provider, updates));
      } finally {
        setBrowser(null);
      }
    });

  if (browser) {
    return (
      <div className="plan-sign-in">
        <p className="muted small">Finish signing in with {browser === "google" ? "Google" : "GitHub"} in your browser, then come back here.</p>
        <div className="place-row">
          <button className="btn" onClick={() => void desktop.planLoginCancel()}>
            Cancel
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="plan-sign-in">
      {!sent && (
        <div className="sign-in-providers">
          <button className="btn" disabled={busy} onClick={() => void withBrowser("google")}>
            Continue with Google
          </button>
          <button className="btn" disabled={busy} onClick={() => void withBrowser("github")}>
            Continue with GitHub
          </button>
          <span className="sign-in-or faint small">or with your email</span>
        </div>
      )}
      {!sent ? (
        <div className="place-row">
          <input
            className="grow"
            type="email"
            placeholder="you@example.com"
            value={email}
            spellCheck={false}
            onChange={(e) => setEmail(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && email.trim() && void send()}
            aria-label="Email"
          />
          <button className="btn primary" disabled={busy || !email.trim()} onClick={() => void send()}>
            {busy ? "Sending…" : "Send code"}
          </button>
          {onCancel && (
            <button className="btn" onClick={onCancel}>
              Cancel
            </button>
          )}
        </div>
      ) : (
        <>
          <p className="muted small">A code is on its way to {email.trim()}. It works for 10 minutes.</p>
          <div className="place-row">
            <input
              className="mono grow"
              inputMode="numeric"
              autoComplete="one-time-code"
              placeholder="6-digit code"
              value={code}
              autoFocus
              onChange={(e) => setCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && code.trim() && void verify()}
              aria-label="Code"
            />
            <button className="btn primary" disabled={busy || !code.trim()} onClick={() => void verify()}>
              {busy ? "Signing in…" : "Sign in"}
            </button>
            <button className="link" onClick={() => (setSent(false), setCode(""), setError(null))}>
              Use another email
            </button>
          </div>
        </>
      )}
      {!sent && (
        <label className="sign-in-updates small">
          <input type="checkbox" checked={updates} onChange={(e) => setUpdates(e.target.checked)} />
          Email me about Marv updates (now and then; unsubscribe any time)
        </label>
      )}
      {error && <p className="notice error">{error}</p>}
    </div>
  );
}

/** The Marv plan's status for a page: what this computer knows first (no wait), then the server's. `checked`: the
 * server's answer (or its error) is in, so "offline" can be said without flashing it while it loads. */
export function usePlanStatus() {
  const [status, setStatus] = useState<PlanStatus | null>(null);
  const [checked, setChecked] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    desktop.planStatus(false).then((s) => setStatus((now) => now ?? s), () => {});
    desktop.planStatus(true).then(
      (s) => (setStatus(s), setChecked(true)),
      (e) => (setError(String(e)), setChecked(true)),
    );
  }, []);
  return { status, setStatus, checked, error };
}

/** Settings › Plan & account: signed out, a line on Pro + models, Sign in and See plans; signed in, the plan, its
 * status pill, the share of this month's allowance used, Manage subscription and Sign out, and at most a few
 * one-line notices. */
export function MarvPlanCard({
  status,
  checked,
  loadError,
  onStatus,
  signingIn,
  onSigningIn,
  onInfo,
  onReloadModels,
}: {
  status: PlanStatus | null;
  checked: boolean;
  loadError: string | null;
  onStatus: (s: PlanStatus) => void;
  signingIn: boolean;
  onSigningIn: (on: boolean) => void;
  onInfo: (info: AppInfo) => void;
  onReloadModels: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  async function act(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await action();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function signedIn(next: PlanStatus) {
    onStatus(next);
    onSigningIn(false);
    onInfo(await desktop.info());
    onReloadModels();
  }

  const signOut = () =>
    act(async () => {
      const out = await desktop.planLogout();
      onInfo(out.info);
      setNotice(out.notice);
      onStatus(await desktop.planStatus(false));
      onReloadModels();
    });

  const takePlanKey = () =>
    act(async () => {
      onInfo(await desktop.planUseKey());
      onStatus(await desktop.planStatus(true));
      onReloadModels();
    });

  if (!status) {
    return (
      <div className="plan-card" aria-busy="true">
        {loadError ? <p className="notice error">{loadError}</p> : <p className="faint small">Checking your plan…</p>}
      </div>
    );
  }
  const view = planView(status);
  const tone = view.usage ? (view.usage.percent >= 100 ? "full" : view.usage.percent >= 80 ? "high" : "") : "";
  return (
    <div className="plan-card">
      <div className="plan-head">
        <h3>{view.title}</h3>
        {view.price && <span className="plan-price">{view.price}</span>}
        {view.status && <span className={`pill ${view.status.tone}`}>{view.status.text}</span>}
        {view.account && (
          <span className="plan-account mono" title={view.account}>
            {view.account}
          </span>
        )}
      </div>
      {view.pitch && <p className="plan-pitch">{view.pitch}</p>}
      {view.usage && (
        <div className="plan-usage" title={view.usage.detail}>
          <div
            className={`plan-bar ${tone}`}
            role="meter"
            aria-label="This month's model allowance used"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={view.usage.percent}
            aria-valuetext={`${view.usage.percent}%, ${view.usage.detail}`}
          >
            <i style={{ width: `${view.usage.percent}%` }} />
          </div>
          <p className="small">{view.usage.text}</p>
        </div>
      )}
      {view.note && <p className="muted small">{view.note}</p>}
      {view.warning && <p className="plan-notice warn">{view.warning}</p>}
      {view.keyChoice && (
        <div className="plan-notice">
          <span className="grow">{view.keyChoice}</span>
          <button className="btn small-btn" disabled={busy} onClick={() => void takePlanKey()} title="Replaces the OpenRouter key on this computer with the plan's">
            Use the Marv plan instead
          </button>
        </div>
      )}
      {view.offline && checked && !loadError && <p className="plan-notice">{view.offline}</p>}
      {status.notice && <p className="plan-notice">{status.notice}</p>}
      {notice && <p className="plan-notice">{notice}</p>}
      {status.signedIn && status.marketing !== null && (
        <label className="sign-in-updates small">
          <input
            type="checkbox"
            checked={status.marketing}
            disabled={busy}
            onChange={(e) => void act(async () => onStatus(await desktop.planSetMarketing(e.target.checked)))}
          />
          Product emails: news about Marv, now and then
        </label>
      )}
      {status.signedIn ? (
        <div className="plan-actions">
          {isFree(status) ? (
            <button className="btn primary" onClick={() => void desktop.openUrl(PRICING_URL)}>
              Upgrade ↗
            </button>
          ) : (
            <button className="btn" disabled={busy} onClick={() => void act(() => desktop.planManage())}>
              Manage subscription
            </button>
          )}
          <button className="btn plain-quiet" disabled={busy} onClick={() => void signOut()}>
            Sign out
          </button>
        </div>
      ) : signingIn ? (
        <MarvPlanSignIn onSignedIn={(s) => void signedIn(s)} onCancel={() => onSigningIn(false)} />
      ) : (
        <div className="plan-actions">
          <button className="btn primary" onClick={() => onSigningIn(true)}>
            Sign in
          </button>
          <button className="link" onClick={() => void desktop.openUrl(PRICING_URL)}>
            See plans ↗
          </button>
        </div>
      )}
      {(error || (loadError && checked)) && <p className="notice error">{error ?? loadError}</p>}
    </div>
  );
}
