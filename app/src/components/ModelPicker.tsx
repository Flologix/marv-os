import { useEffect, useState } from "react";
import { type Model, desktop, isDesktop, localModels } from "../lib/backend";
import { tokens } from "../lib/format";

/** Models Marv runs well on this machine: tool calls, and they fit a 12 GB GPU. */
const SUGGESTED = [
  { id: "gemma4:12b", note: "8 GB · tools, thinking" },
  { id: "qwen3:14b", note: "9 GB · tools, thinking" },
];

/** Where Marv's model comes from: OpenRouter (cloud, a key), a plan set up here (Z.ai Coding Plan, Kimi Code plan, ChatGPT), or Ollama
 * (this machine, free). */
export function ModelPicker({
  model,
  remote,
  hasKey,
  onSetupPlan,
  onPick,
  recommended = [],
  initialTab,
  autoFocus = false,
}: {
  model: string;
  remote: Model[];
  hasKey: boolean;
  /** #159: set up a plan (the Z.ai Coding Plan, the Kimi Code plan, Sign in with ChatGPT) when this build offers one. */
  onSetupPlan?: () => void;
  onPick: (id: string) => void;
  /** Listed first on the OpenRouter tab while nothing is searched (the Marv plan's default model), with their note. */
  recommended?: { id: string; note: string }[];
  /** The tab to open on; by default the current model's. */
  initialTab?: "openrouter" | "plan" | "ollama";
  /** Focus the search when it opens (Settings' picker opens on a click). */
  autoFocus?: boolean;
}) {
  const plans = remote.filter((m) => m.plan);
  const [tab, setTab] = useState<"openrouter" | "plan" | "ollama">(
    initialTab ??
    model.startsWith("ollama/") ? "ollama" : plans.some((m) => m.id === model) ? "plan" : "openrouter",
  );
  const [query, setQuery] = useState("");
  const [local, setLocal] = useState<Model[] | null>(null);
  const [pulling, setPulling] = useState<string | null>(null);
  const [progress, setProgress] = useState("");
  const [error, setError] = useState<string | null>(null);

  const refresh = () =>
    desktop.localModels().then(
      () => localModels().then(setLocal),
      (e) => {
        setLocal([]);
        setError(String(e));
      },
    );

  useEffect(() => {
    if (tab === "ollama" && isDesktop) void refresh();
  }, [tab]);

  async function pull(name: string) {
    setError(null);
    setPulling(name);
    setProgress("starting…");
    try {
      await desktop.pullModel(name, setProgress);
      await refresh();
      onPick(`ollama/${name}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setPulling(null);
    }
  }

  const q = query.trim().toLowerCase();
  const open = remote.filter((m) => !m.plan);
  const notes = new Map(recommended.map((r) => [r.id, r.note]));
  const first = q ? [] : recommended.map((r) => open.find((m) => m.id === r.id)).filter((m): m is Model => !!m);
  const shown = [
    ...first,
    ...open.filter((m) => !first.includes(m)).filter((m) => !q || m.id.toLowerCase().includes(q) || m.name.toLowerCase().includes(q)),
  ].slice(0, 80);

  return (
    <div className="picker">
      <div className="picker-tabs" role="tablist">
        <button role="tab" aria-selected={tab === "openrouter"} className={tab === "openrouter" ? "active" : ""} onClick={() => setTab("openrouter")}>
          OpenRouter
        </button>
        {(plans.length > 0 || onSetupPlan) && (
          <button role="tab" aria-selected={tab === "plan"} className={tab === "plan" ? "active" : ""} onClick={() => setTab("plan")}>
            Your plan
          </button>
        )}
        {isDesktop && (
          <button role="tab" aria-selected={tab === "ollama"} className={tab === "ollama" ? "active" : ""} onClick={() => setTab("ollama")}>
            Ollama · local
          </button>
        )}
      </div>

      {tab === "plan" ? (
        <>
          <p className="help">
            {plans.length > 0
              ? "Paid by your plan, within its usage limits. When a limit is reached Marv stops and says so."
              : "Use a plan you already pay for: the Z.ai GLM Coding Plan, the Kimi Code plan, or your ChatGPT plan."}
          </p>
          <div className="model-list">
            {plans.map((m) => (
              <button key={m.id} className={`model${m.id === model ? " active" : ""}`} onClick={() => onPick(m.id)} title={m.id}>
                <span className="model-id">{m.name}</span>
                <span className="model-meta">
                  {m.context ? `${tokens(m.context)} · ` : ""}
                  {m.plan}
                </span>
              </button>
            ))}
          </div>
          {onSetupPlan && (
            <button className="btn" onClick={onSetupPlan}>
              {plans.length > 0 ? "Set up another plan…" : "Set up a plan…"}
            </button>
          )}
        </>
      ) : tab === "openrouter" ? (
        <>
          {!hasKey && <p className="help warn">Add an OpenRouter key (first-run setup) to use these.</p>}
          <input
            className="search mono"
            placeholder={`search ${open.length} models`}
            value={query}
            spellCheck={false}
            autoFocus={autoFocus}
            onChange={(e) => setQuery(e.target.value)}
            aria-label="Search models"
          />
          <div className="model-list">
            {shown.map((m) => (
              <button
                key={m.id}
                className={`model${m.id === model ? " active" : ""}${notes.has(m.id) && !q ? " recommended" : ""}`}
                onClick={() => onPick(m.id)}
                title={`${m.name} · $ per million tokens in/out`}
              >
                <span className="model-id">
                  {m.id}
                  {notes.has(m.id) && !q && <span className="model-tag">{notes.get(m.id)}</span>}
                </span>
                <span className="model-meta">
                  {tokens(m.context)} · ${m.input.toFixed(2)}/${m.output.toFixed(2)}
                </span>
              </button>
            ))}
            {shown.length === 0 && <p className="help">No match.</p>}
          </div>
        </>
      ) : (
        <>
          <p className="help">Runs on this machine: free, private, and works offline. The first message loads it into memory.</p>
          <div className="model-list">
            {local?.map((m) => (
              <button key={m.id} className={`model${m.id === model ? " active" : ""}`} onClick={() => onPick(m.id)}>
                <span className="model-id">{m.name}</span>
                <span className="model-meta">local</span>
              </button>
            ))}
            {local && local.length === 0 && !error && <p className="help">No models yet: download one below.</p>}
          </div>
          <div className="pull">
            <span className="help">Download</span>
            {SUGGESTED.filter((s) => !local?.some((m) => m.id === `ollama/${s.id}`)).map((s) => (
              <button key={s.id} className="btn" disabled={pulling !== null} onClick={() => void pull(s.id)}>
                {s.id}&nbsp;<span className="faint">{s.note}</span>
              </button>
            ))}
          </div>
          {pulling && (
            <p className="help mono">
              {pulling}: {progress}
            </p>
          )}
        </>
      )}
      {error && <p className="notice error">{error}</p>}
    </div>
  );
}
