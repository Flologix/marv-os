import { useEffect, useState } from "react";
import type { ChatSettings, ChatWorktree, Model } from "../lib/backend";
import type { ContextBudget } from "../lib/chat";
import { tokens } from "../lib/format";
import { type ChatDevices, DeviceSwitches } from "./Devices";
import { ModelPicker } from "./ModelPicker";

/** The smallest and largest context budget (marv-session's MIN_COMPACT_TOKENS and MAX_CONTEXT_BUDGET). */
const MIN_BUDGET = 40_000;
const MAX_BUDGET = 10_000_000;

export function Settings({
  settings,
  models,
  hasKey = true,
  onSetupPlan,
  onChange,
  devices,
  onClose,
  budget,
  onBudget,
  worktree,
  onMoveBack,
}: {
  /** Runtime-kernel §14.1: the chat's own worktree, when it works in one. */
  worktree?: ChatWorktree | null;
  onMoveBack?: () => void;
  settings: ChatSettings;
  models: Model[];
  hasKey?: boolean;
  onSetupPlan?: () => void;
  onChange: (s: ChatSettings) => void;
  /** This chat's devices and which are off (a desktop chat only). */
  devices?: ChatDevices;
  onClose: () => void;
  /** Runtime-kernel §11.2: this chat's context budget, set only here (by you), never by the model or a project. */
  budget?: ContextBudget;
  onBudget?: (tokens: number | null) => void;
}) {
  return (
    <div className="popover" role="dialog" aria-label="Settings">
      <div className="field">
        <span>Model</span>
        <ModelPicker
          model={settings.model}
          remote={models.filter((m) => !m.local)}
          hasKey={hasKey}
          onSetupPlan={onSetupPlan}
          onPick={(model) => model !== settings.model && onChange({ ...settings, model })}
        />
      </div>
      <label className="field">
        <span>Steps before asking to continue</span>
        <input
          type="number"
          min={0}
          max={500}
          style={{ width: 96 }}
          value={settings.maxSteps}
          onChange={(e) => onChange({ ...settings, maxSteps: Math.max(0, Math.min(500, Number(e.target.value) || 0)) })}
        />
        <span className="help">0 never asks.</span>
      </label>
      {onBudget && (
        <label className="field">
          <span>Context budget (tokens)</span>
          <BudgetField budget={budget} onBudget={onBudget} />
        </label>
      )}
      <div className="switch">
        <button
          type="button"
          role="switch"
          aria-checked={settings.yolo}
          aria-labelledby="yolo"
          className={`toggle${settings.yolo ? " on" : ""}`}
          onClick={() => onChange({ ...settings, yolo: !settings.yolo })}
        />
        <div>
          <div id="yolo">Run sandboxed work without asking</div>
          <div className="help">Off: approve every edit and command, too.</div>
        </div>
      </div>
      {worktree && onMoveBack && (
        <div className="field">
          <span>Works in its own worktree</span>
          <span className="help" title={worktree.dir}>
            Branch {worktree.branch}, from {worktree.base}. Merge it when you're happy with it.
          </span>
          <button className="btn" onClick={onMoveBack}>
            Move back to the folder
          </button>
        </div>
      )}
      {devices && (
        <div className="field">
          <span>Devices</span>
          <DeviceSwitches devices={devices} />
        </div>
      )}
      <div style={{ display: "flex", justifyContent: "flex-end" }}>
        <button className="btn" onClick={onClose}>
          Done
        </button>
      </div>
    </div>
  );
}

/**
 * Runtime-kernel §11.2: this chat's context budget, set only here (by you), never by the model or a project. In the
 * chat's settings and its dock's context tab (kernel #53).
 */
export function BudgetField({ budget, onBudget }: { budget?: ContextBudget; onBudget: (tokens: number | null) => void }) {
  const [draft, setDraft] = useState(budget?.set ? String(budget.set) : "");
  // The budget in use, again when it changes or a change was refused.
  useEffect(() => setDraft(budget?.set ? String(budget.set) : ""), [budget?.set, budget?.epoch]);
  const typed = Number(draft);
  const bad = draft !== "" && (!Number.isSafeInteger(typed) || typed < MIN_BUDGET || typed > MAX_BUDGET);
  function commit() {
    if (bad) return;
    const next = draft === "" ? null : typed;
    if (next !== (budget?.set ?? null)) onBudget(next);
  }
  return (
    <>
      <input
        type="number"
        min={MIN_BUDGET}
        max={MAX_BUDGET}
        step={10_000}
        style={{ width: 120 }}
        placeholder="default"
        value={draft}
        aria-invalid={bad}
        aria-label="Context budget (tokens)"
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => e.key === "Enter" && commit()}
      />
      <span className="help">
        {bad
          ? `A whole number from ${tokens(MIN_BUDGET)} to ${tokens(MAX_BUDGET)}.`
          : budget?.error
            ? budget.error
            : `Marv compacts the conversation here${budget?.budget ? ` (now ${tokens(budget.budget)})` : ""}. Empty: your default, never past 85% of the model's window.`}
      </span>
    </>
  );
}
