import { open } from "@tauri-apps/plugin-dialog";
import { type ReactNode, useEffect, useState } from "react";
import { type AppInfo, type AppStatus, type LocalModel, desktop } from "../lib/backend";
import { Gallery } from "./Gallery";
import { Martian } from "./Martian";
import { MarvPlanSignIn } from "./MarvPlan";
import { AccountGate } from "./AccountGate";
import { LINUX } from "../lib/platform";
import { SKIPPED_KEY, brainSkipped } from "../lib/setup";
import { PRICING_URL, type PlanStatus, planName } from "../lib/plan";

const SUGGESTED = ["gemma4:12b", "qwen3:14b"];

/**
 * First run, three steps: how Marv thinks (the Marv plan, OpenRouter in the cloud, Ollama on this machine, or a plan
 * you already pay for when this build offers it: the Z.ai GLM Coding Plan, Kimi Code, Sign in with ChatGPT), the first space's
 * folder, and the apps to connect (skippable). Each step shows only while it's needed.
 */
export function Onboarding({
  info,
  onInfo,
  onModel,
  onFinish,
  again = false,
}: {
  info: AppInfo;
  onInfo: (info: AppInfo) => void;
  onModel: (model: string) => void;
  onFinish: () => void;
  /** #159: only the first step again (from the model menu, to set up a plan), with a way back. */
  again?: boolean;
}) {
  // Kept in this window's storage: the app asks for a provider on every start otherwise (`App` reads it too).
  const [skipped, setSkipped] = useState(brainSkipped);
  const skipBrain = () => {
    try {
      localStorage.setItem(SKIPPED_KEY, "1");
    } catch {}
    setSkipped(true);
  };
  const step = again
    ? "brain"
    : !info.signedIn && !info.accountAsked
      ? "account"
      : !info.ready && !skipped
        ? "brain"
        : !info.root
          ? "folder"
          : "apps";
  const [installed, setInstalled] = useState<AppStatus[]>([]);
  const refresh = () => desktop.appsStatus().then(setInstalled, () => {});

  useEffect(() => {
    if (step === "apps") void refresh();
  }, [step]);

  return (
    <div className="onboarding">
      <div className={`card${step === "apps" ? " wide" : ""}`}>
        <Martian size={56} className="toon" />
        <div className="brand">
          <img className="flai" src="/flai.png" alt="FloLogix AI" /> marv desktop
          {!again && (
            <span className="steps">
              {["account", "brain", "folder", "apps"].map((s) => (
                <i key={s} className={s === step ? "on" : ""} />
              ))}
            </span>
          )}
        </div>
        {step === "account" && <Account onInfo={onInfo} onFinish={onFinish} />}
        {step === "brain" && <Brain info={info} onInfo={onInfo} onModel={onModel} onSkip={again ? undefined : skipBrain} />}
        {again && (
          <div>
            <button className="btn" onClick={onFinish}>
              Back
            </button>
          </div>
        )}
        {step === "folder" && <Folder onInfo={onInfo} />}
        {step === "apps" && (
          <>
            <h1>Connect your apps</h1>
            <p>Sign Marv in to the tools you work in: your mail and calendar, Slack, Notion, GitHub. Each opens its own sign-in page in your browser. Add more later from Apps.</p>
            <AccountGate info={info} onInfo={onInfo} feature="Apps" pitch="Sign Marv in to your mail, calendar, Slack, Notion and GitHub, for every chat to use.">
              <Gallery mine={installed} onConnected={() => void refresh()} />
            </AccountGate>
            <div>
              <button className="btn primary" onClick={onFinish}>
                {installed.length ? "Done" : "Skip for now"}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

/** First run: sign in to a free Marv account (Google, GitHub or an emailed code), or skip it. Shown once; someone
 * already set up before accounts existed sees only this step. */
function Account({ onInfo, onFinish }: { onInfo: (info: AppInfo) => void; onFinish: () => void }) {
  const [error, setError] = useState<string | null>(null);
  const done = () =>
    desktop.accountStepDone().then(
      (next) => {
        onInfo(next);
        if (next.ready && next.root) onFinish();
      },
      (e) => setError(String(e)),
    );
  return (
    <>
      <h1>Sign in to Marv</h1>
      <p>A free account keeps your plan with you on every computer, and lets you upgrade later. Marv works the same without one.</p>
      <MarvPlanSignIn onSignedIn={() => void done()} />
      {error && <p className="notice error">{error}</p>}
      <div>
        <button className="btn plain-quiet" onClick={() => void done()}>
          Skip for now
        </button>
      </div>
    </>
  );
}

type ChoiceId = "marv" | "openrouter" | "ollama" | "zai" | "kimi" | "chatgpt";

/** One way for Marv to think, as a card: its title, one line, and (open) its form. Every card has the same shape;
 * only the open one shows its form. */
function Choice({
  id,
  title,
  tag,
  line,
  state,
  open,
  onOpen,
  children,
}: {
  id: ChoiceId;
  title: string;
  tag?: string;
  line: string;
  /** A short state word on the right (Running, Signed in…). */
  state?: string;
  open: boolean;
  onOpen: (id: ChoiceId | null) => void;
  children: ReactNode;
}) {
  return (
    <section className={`choice${open ? " open" : ""}`}>
      <button className="choice-head" aria-expanded={open} aria-controls={`choice-${id}`} onClick={() => onOpen(open ? null : id)}>
        <span className="grow choice-text">
          <span className="choice-title">
            <b>{title}</b>
            {tag && <span className="pill ok">{tag}</span>}
          </span>
          <span className="help">{line}</span>
        </span>
        {state && <span className="faint small choice-state">{state}</span>}
        <span className="choice-chev" aria-hidden="true">
          {open ? "−" : "+"}
        </span>
      </button>
      {open && (
        <div className="choice-body" id={`choice-${id}`}>
          {children}
        </div>
      )}
    </section>
  );
}

function Brain({
  info,
  onInfo,
  onModel,
  onSkip,
}: {
  info: AppInfo;
  onInfo: (info: AppInfo) => void;
  onModel: (model: string) => void;
  /** First run only: go on without a provider (Claude Code and Codex bring their own). */
  onSkip?: () => void;
}) {
  const [local, setLocal] = useState<LocalModel[] | null>(null);
  const [ollamaError, setOllamaError] = useState<string | null>(null);
  const [key, setKey] = useState("");
  const [pulling, setPulling] = useState<string | null>(null);
  const [progress, setProgress] = useState("");
  const [error, setError] = useState<string | null>(null);
  // Which way is open: the one that works for this person (`undefined`: not picked yet). A Marv plan with models
  // when they signed in with one; otherwise Ollama when it already has a model, else OpenRouter. The Marv plan card
  // used to open first and ask for a sign-in again, right after the account step, even for a free account.
  const [picked, setPicked] = useState<ChoiceId | null | undefined>(undefined);
  const [plan, setPlan] = useState<PlanStatus | null>(null);
  useEffect(() => {
    if (info.signedIn) desktop.planStatus(true).then(setPlan, () => {});
  }, [info.signedIn]);

  const check = () =>
    desktop.localModels().then(
      (m) => (setLocal(m), setOllamaError(null)),
      (e) => (setLocal(null), setOllamaError(String(e))),
    );

  useEffect(() => {
    void check();
  }, []);

  async function chooseLocal(id: string) {
    try {
      onModel(id);
      onInfo(await desktop.setProvider("ollama"));
    } catch (e) {
      setError(String(e));
    }
  }

  async function pull(name: string) {
    setPulling(name);
    try {
      await desktop.pullModel(name, setProgress);
      await chooseLocal(`ollama/${name}`);
    } catch (e) {
      setError(String(e));
      setPulling(null);
    }
  }

  async function saveKey() {
    try {
      await desktop.setApiKey(key);
      onInfo(await desktop.setProvider("openrouter"));
    } catch (e) {
      setError(String(e));
    }
  }

  const usable = local?.filter((m) => m.tools) ?? [];
  const plans = info.offerZai || info.offerKimi || info.offerChatgpt;
  const open: ChoiceId | null = picked !== undefined ? picked : plan?.hasModels ? "marv" : usable.length ? "ollama" : "openrouter";
  const setOpen = (id: ChoiceId | null) => setPicked(id);
  return (
    <>
      <h1>How should Marv think?</h1>
      <p>Pick where its model runs. You can switch any time from the model menu or Settings.</p>
      <div className="choices">
        <MarvPlanChoice onInfo={onInfo} onModel={onModel} open={open} onOpen={setOpen} known={plan} />
        <Choice id="openrouter" title="OpenRouter" line="Your own key: hundreds of models, pay per use. Fast on any computer." open={open === "openrouter"} onOpen={setOpen}>
          <input
            className="mono"
            type="password"
            placeholder="sk-or-…"
            value={key}
            autoFocus
            onChange={(e) => setKey(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && key.trim() && void saveKey()}
            aria-label="OpenRouter key"
          />
          <span className="help">
            From{" "}
            <button className="link" onClick={() => void desktop.openUrl("https://openrouter.ai/settings/keys")}>
              openrouter.ai/settings/keys ↗
            </button>
            . Stays on this machine.
          </span>
          <div className="choice-actions">
            <button className="btn primary" disabled={!key.trim()} onClick={() => void saveKey()}>
              Use OpenRouter
            </button>
          </div>
        </Choice>
        <Choice
          id="ollama"
          title="On this computer · Ollama"
          line="Free and private; nothing leaves your machine. Needs a good GPU for speed."
          state={ollamaError ? "Not running" : usable.length ? `${usable.length} ready` : local ? "Running" : undefined}
          open={open === "ollama"}
          onOpen={setOpen}
        >
          {ollamaError ? (
            <>
              {LINUX ? (
                <>
                  <span className="help warn">Ollama isn't running. Install it, then start it:</span>
                  <pre className="code">sudo systemctl enable --now ollama</pre>
                </>
              ) : (
                <span className="help warn">Ollama isn't running. Install it from ollama.com, then start it.</span>
              )}
              <div className="choice-actions">
                <button className="btn" onClick={() => void check()}>
                  Check again
                </button>
              </div>
            </>
          ) : (
            <>
              <div className="choice-actions">
                {usable.map((m) => (
                  <button key={m.id} className="btn primary" onClick={() => void chooseLocal(m.id)}>
                    Use {m.id.slice(7)}
                  </button>
                ))}
                {SUGGESTED.filter((s) => !local?.some((m) => m.id === `ollama/${s}`)).map((s) => (
                  <button key={s} className="btn" disabled={pulling !== null} onClick={() => void pull(s)}>
                    Download {s}
                  </button>
                ))}
              </div>
              {pulling && (
                <span className="help mono">
                  {pulling}: {progress}
                </span>
              )}
            </>
          )}
        </Choice>
      </div>
      {plans && <Plans info={info} onInfo={onInfo} onModel={onModel} onError={setError} open={open} onOpen={setOpen} />}
      {error && <p className="error">{error}</p>}
      {onSkip && (
        <div>
          <button className="btn plain-quiet" title="Claude Code and Codex use their own logins. Marv's own chats need a provider: add one later from Settings." onClick={onSkip}>
            Skip for now
          </button>
        </div>
      )}
    </>
  );
}

/** The Marv plan: Pro + models brings an OpenRouter key with a monthly allowance (saved where an OpenRouter key lives,
 * never over the person's own); Founding Member has no models. */
function MarvPlanChoice({
  onInfo,
  onModel,
  open,
  onOpen,
  known,
}: {
  onInfo: (info: AppInfo) => void;
  onModel: (model: string) => void;
  open: ChoiceId | null;
  onOpen: (id: ChoiceId | null) => void;
  /** The account the person signed in with at the account step, once its plan is known. */
  known: PlanStatus | null;
}) {
  const [status, setStatus] = useState<PlanStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Signed in already: no second sign-in. A plan with models is used at once (the step then ends by itself).
  useEffect(() => {
    if (known?.signedIn) void use(known);
  }, [known]); // eslint-disable-line react-hooks/exhaustive-deps

  async function use(next: PlanStatus) {
    setStatus(next);
    setError(null);
    try {
      if (next.hasModels && next.keyInUse) {
        if (next.allowance?.defaultModel) onModel(next.allowance.defaultModel);
        onInfo(await desktop.setProvider("openrouter"));
      }
    } catch (e) {
      setError(String(e));
    }
  }

  async function choose(planKey: boolean) {
    setError(null);
    try {
      if (planKey) {
        await desktop.planUseKey();
        const next = await desktop.planStatus(true);
        setStatus(next);
        if (next.allowance?.defaultModel) onModel(next.allowance.defaultModel);
      }
      onInfo(await desktop.setProvider("openrouter"));
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <Choice
      id="marv"
      title="Marv plan"
      tag="No key needed"
      line="Pro + models: DeepSeek V4.1 Flash and other fast coding models included, nothing to set up. The easiest way to start."
      state={status ? planName(status.plan) : undefined}
      open={open === "marv"}
      onOpen={onOpen}
    >
      {!status ? (
        <>
          <span className="help">Sign in with the email you subscribed with.</span>
          <MarvPlanSignIn onSignedIn={(s) => void use(s)} />
          <span className="help">
            No plan yet?{" "}
            <button className="link" onClick={() => void desktop.openUrl(PRICING_URL)}>
              See plans ↗
            </button>
          </span>
        </>
      ) : !status.hasModels ? (
        <span className="help">
          Signed in as {status.email ?? "you"} ({planName(status.plan)}). Models come with Pro + models:{" "}
          <button className="link" onClick={() => void desktop.openUrl(PRICING_URL)}>
            see plans ↗
          </button>
          , or pick one of the ways below.
        </span>
      ) : status.ownKey ? (
        <>
          <span className="help">You already have an OpenRouter key here. Use the plan's allowance instead?</span>
          <div className="choice-actions">
            <button className="btn primary" onClick={() => void choose(true)}>
              Use the Marv plan instead
            </button>
            <button className="btn" onClick={() => void choose(false)}>
              Keep my key
            </button>
          </div>
        </>
      ) : (
        <span className="help">Signed in: {planName(status.plan)}.</span>
      )}
      {status?.notice && <span className="help warn">{status.notice}</span>}
      {error && <span className="help warn">{error}</span>}
    </Choice>
  );
}

/** A plan the person already pays for, when this build offers it: the Z.ai GLM Coding Plan and the Kimi Code plan
 * (each its own key), Sign in with ChatGPT (the browser). */
function Plans({
  info,
  onInfo,
  onModel,
  onError,
  open,
  onOpen,
}: {
  info: AppInfo;
  onInfo: (info: AppInfo) => void;
  onModel: (model: string) => void;
  onError: (error: string | null) => void;
  open: ChoiceId | null;
  onOpen: (id: ChoiceId | null) => void;
}) {
  const [zaiKey, setZaiKey] = useState("");
  const [kimiKey, setKimiKey] = useState("");
  const [signingIn, setSigningIn] = useState(false);
  // The sign-in's outcome, shown in its own card: the page's error line sits below every choice, out of sight.
  const [chatgptError, setChatgptError] = useState<string | null>(null);

  async function chooseZai() {
    try {
      await desktop.setZaiKey(zaiKey);
      onModel("zai/glm-5.3");
      onInfo(await desktop.setProvider("zai"));
    } catch (e) {
      onError(String(e));
    }
  }

  async function chooseKimi() {
    try {
      await desktop.setKimiKey(kimiKey);
      onModel("kimi/kimi-for-coding");
      onInfo(await desktop.setProvider("kimi"));
    } catch (e) {
      onError(String(e));
    }
  }

  async function chooseChatgpt(another = false) {
    onError(null);
    setChatgptError(null);
    setSigningIn(true);
    try {
      if (another || !info.chatgptAccount) await desktop.chatgptSignIn(another);
      const first = (await desktop.planModels()).find((m) => m.id.startsWith("chatgpt/"));
      if (first) onModel(first.id);
      onInfo(await desktop.setProvider("chatgpt"));
    } catch (e) {
      setChatgptError(String(e));
    } finally {
      setSigningIn(false);
    }
  }

  return (
    <>
      <p className="choices-group">Or use a plan you already pay for</p>
      <div className="choices">
        {info.offerZai && (
          <Choice
            id="zai"
            title="Z.ai GLM Coding Plan"
            line="GLM-5.3 on the plan you subscribe to; its usage limits apply."
            state={info.hasZaiKey ? "Key saved" : undefined}
            open={open === "zai"}
            onOpen={onOpen}
          >
            <input className="mono" type="password" placeholder="Coding Plan API key" value={zaiKey} autoFocus onChange={(e) => setZaiKey(e.target.value)} aria-label="Coding Plan API key" />
            <span className="help">
              From{" "}
              <button className="link" onClick={() => void desktop.openUrl("https://z.ai/manage-apikey/apikey-list")}>
                z.ai/manage-apikey/apikey-list ↗
              </button>
              . Stays on this machine.
            </span>
            <div className="choice-actions">
              <button className="btn primary" disabled={!zaiKey.trim() && !info.hasZaiKey} onClick={() => void chooseZai()}>
                Use the Coding Plan
              </button>
            </div>
          </Choice>
        )}
        {info.offerKimi && (
          <Choice
            id="kimi"
            title="Kimi Code plan"
            line="Kimi's coding models on the plan you subscribe to; its usage limits apply."
            state={info.hasKimiKey ? "Key saved" : undefined}
            open={open === "kimi"}
            onOpen={onOpen}
          >
            <input className="mono" type="password" placeholder="Kimi Code API key" value={kimiKey} autoFocus onChange={(e) => setKimiKey(e.target.value)} aria-label="Kimi Code API key" />
            <span className="help">
              From{" "}
              <button className="link" onClick={() => void desktop.openUrl("https://www.kimi.com/code/console")}>
                kimi.com/code/console ↗
              </button>
              . Stays on this machine.
            </span>
            <div className="choice-actions">
              <button className="btn primary" disabled={!kimiKey.trim() && !info.hasKimiKey} onClick={() => void chooseKimi()}>
                Use the Kimi plan
              </button>
            </div>
          </Choice>
        )}
        {info.offerChatgpt && (
          <Choice
            id="chatgpt"
            title="Sign in with ChatGPT"
            line="Your ChatGPT Plus or Pro plan pays for Marv, within its usage limits."
            state={info.chatgptAccount ? "Signed in" : undefined}
            open={open === "chatgpt"}
            onOpen={onOpen}
          >
            <span className="help">{info.chatgptAccount ? `Signed in as ${info.chatgptAccount}.` : "Your browser opens to sign in."}</span>
            {chatgptError && <span className="help warn">{chatgptError}</span>}
            <div className="choice-actions">
              <button className="btn primary" disabled={signingIn} onClick={() => void chooseChatgpt()}>
                {signingIn ? "Waiting for the browser…" : info.chatgptAccount ? "Use ChatGPT" : "Sign in with ChatGPT"}
              </button>
              {signingIn && (
                <button className="btn" onClick={() => void desktop.chatgptCancelSignIn()}>
                  Cancel
                </button>
              )}
              {info.chatgptAccount && !signingIn && (
                <button className="link" onClick={() => void chooseChatgpt(true)}>
                  Use another ChatGPT account
                </button>
              )}
            </div>
          </Choice>
        )}
      </div>
    </>
  );
}

function Folder({ onInfo }: { onInfo: (info: AppInfo) => void }) {
  const [error, setError] = useState<string | null>(null);
  async function choose() {
    setError(null);
    const path = await open({ directory: true, title: "Choose your first space's folder" });
    if (typeof path !== "string") return;
    try {
      onInfo(await desktop.setRoot(path));
    } catch (e) {
      setError(String(e));
    }
  }
  return (
    <>
      <h1>Choose a folder</h1>
      <p>
        Your first space: Marv can read and change everything in this folder, and asks before anything outside it. Make more spaces later
        (a project, your work inbox…) with the + at the top.
      </p>
      <p className="faint mono" style={{ fontSize: 12 }}>
        e.g. ~/Projects or a new ~/Marv. Not your home folder itself.
      </p>
      <div>
        <button className="btn primary" onClick={() => void choose()}>
          Choose folder…
        </button>
      </div>
      {error && <p className="error">{error}</p>}
    </>
  );
}
