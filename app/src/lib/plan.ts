// The Marv plan (Founding Member, Pro + models) as the page shows it: the words for each state, kept here so they can
// be tested without a window. The server's words reach the page as plain text only (React escapes them).

export const PRICING_URL = "https://www.flologixai.com/marv-os/pricing";

export interface PlanAllowance {
  limitUsd: number;
  usedUsd: number;
  /** ISO time; the 1st of next month 00:00 UTC by the contract. */
  resetsAt: string | null;
  defaultModel: string | null;
  /** The models the plan's key may call; empty from an older server (then nothing is narrowed). */
  allowedModels: string[];
}

/** `plan_status` (app/src-tauri/src/plan.rs). The plan's key itself never reaches the page. */
export interface PlanStatus {
  signedIn: boolean;
  email: string | null;
  plan: string | null;
  /** "active", "past_due" or "canceled"; null offline. */
  status: string | null;
  periodEnd: number | null;
  /** Online: what the subscription costs, in whole currency units. */
  price: { amount: number; currency: string; interval: string } | null;
  licence: "valid" | "expired" | "invalid" | "none";
  licenceExpires: number | null;
  /** From the licence alone: "founding", "pro_models" or "free". */
  effective: string;
  allowance: PlanAllowance | null;
  hasModels: boolean;
  keyInUse: boolean;
  /** The models the menu offers from OpenRouter while the plan's key is in use (empty: not narrowed), online or not. */
  menuModels: string[];
  ownKey: boolean;
  online: boolean;
  notice: string | null;
  /** Online: whether the person agreed to product emails. */
  marketing: boolean | null;
}

export function planName(plan: string | null | undefined): string {
  if (plan === "founding") return "Founding Member";
  if (plan === "pro_models") return "Pro + models";
  return "Free";
}

const money = (usd: number) => `$${usd.toFixed(2)}`;

/** "Dec 1", in UTC (the allowance resets at 00:00 UTC). */
export function shortDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" });
}

/** The 1st of next month, 00:00 UTC: when the allowance resets. */
export function nextReset(now: Date): string {
  return new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1)).toISOString();
}

/** "$20/month", "$96/year"; null when there's no price or it can't be shown (an unknown currency or interval). */
export function priceLabel(price: PlanStatus["price"] | undefined): string | null {
  if (!price || !Number.isFinite(price.amount) || price.amount < 0) return null;
  if (price.interval !== "month" && price.interval !== "year") return null;
  let amount: string;
  try {
    amount = new Intl.NumberFormat("en-US", {
      style: "currency",
      currency: price.currency.toUpperCase(),
      minimumFractionDigits: Number.isInteger(price.amount) ? 0 : 2,
    }).format(price.amount);
  } catch {
    return null; // Not an ISO currency code.
  }
  return `${amount}/${price.interval}`;
}

/** The share of this month's allowance used, as a whole percent from 0 to 100; null when there's no allowance to
 * measure against (a limit of 0 or none). */
export function allowancePercent(usedUsd: number | null | undefined, limitUsd: number | null | undefined): number | null {
  if (typeof limitUsd !== "number" || !Number.isFinite(limitUsd) || limitUsd <= 0) return null;
  const used = typeof usedUsd === "number" && Number.isFinite(usedUsd) ? usedUsd : 0;
  return Math.min(100, Math.max(0, Math.round((used / limitUsd) * 100)));
}

/** The allowance as the plan card shows it: the bar's percent, its line, and the dollars for a tooltip. */
export interface PlanUsage {
  percent: number;
  /** "16% of this month's allowance used · resets Dec 1". */
  text: string;
  /** "$1.25 of $8.00 used · $6.75 left": secondary, shown on hover. */
  detail: string;
}

/** What Settings › Plan & account shows. */
export interface PlanView {
  /** The card's heading: the plan's name, or "Marv plan" signed out. */
  title: string;
  /** The account's email (signed in). */
  account: string | null;
  /** What the plan costs, "$20/month"; online only. */
  price: string | null;
  /** The status pill; none offline (the server's word isn't known). */
  status: { text: string; tone: "ok" | "warn" | "off" } | null;
  /** Signed out: what Pro + models gives. */
  pitch: string | null;
  /** A line in place of the allowance when the plan has no models. */
  note: string | null;
  usage: PlanUsage | null;
  /** A warning line (a payment due, a licence this computer can't confirm). */
  warning: string | null;
  /** The plan's key isn't the one in use: the words beside "Use the Marv plan instead". */
  keyChoice: string | null;
  /** Signed in but the server wasn't reached. */
  offline: string | null;
}

const STATUS: Record<string, { text: string; tone: "ok" | "warn" | "off" }> = {
  active: { text: "Active", tone: "ok" },
  past_due: { text: "Payment due", tone: "warn" },
  canceled: { text: "Canceled", tone: "off" },
};

export const PLAN_PITCH = "Pro + models gives you fast coding models, DeepSeek V4.1 Flash first, with a monthly allowance and no key of your own to set up.";

/** A free account (signed in, no plan): one line on what upgrading adds. */
export const FREE_NOTE = "Free: every agent, with your own keys and subscriptions. Upgrade for a monthly model allowance.";

export function planView(s: PlanStatus): PlanView {
  if (!s.signedIn) {
    return {
      title: "Marv account",
      account: null,
      price: null,
      status: { text: "Not signed in", tone: "off" },
      pitch: PLAN_PITCH,
      note: null,
      usage: null,
      warning: null,
      keyChoice: null,
      offline: null,
    };
  }
  if (isFree(s)) {
    return {
      title: "Free",
      account: s.email,
      price: null,
      status: null,
      pitch: null,
      note: FREE_NOTE,
      usage: null,
      warning: null,
      keyChoice: null,
      offline: s.online ? null : "Offline: this is what this computer last knew.",
    };
  }
  const status = s.status ? (STATUS[s.status] ?? { text: s.status, tone: "off" as const }) : null;
  const a = s.allowance;
  let usage: PlanUsage | null = null;
  const percent = a ? allowancePercent(a.usedUsd, a.limitUsd) : null;
  if (a && percent !== null) {
    const left = Math.max(0, a.limitUsd - a.usedUsd);
    const reset = a.resetsAt ? ` · resets ${shortDate(a.resetsAt)}` : "";
    usage = {
      percent,
      text: `${percent}% of this month's allowance used${reset}`,
      detail: `${money(a.usedUsd)} of ${money(a.limitUsd)} used · ${money(left)} left`,
    };
  }
  const note = !s.hasModels && !a ? "Backs Marv early; models aren't included: Marv uses the connection you pick under Model." : null;
  let warning: string | null = null;
  if (s.status === "past_due") warning = "Your last payment didn't go through. Update it in Manage subscription.";
  else if (s.effective === "free" && s.licence === "expired") warning = "The plan's licence has expired, so this computer counts it as Free.";
  else if (s.effective === "free") warning = "This computer couldn't confirm the plan's licence, so it counts it as Free.";
  let keyChoice: string | null = null;
  // Only the server's exact key says which key is the plan's: offline, no choice is offered.
  if (s.online && s.hasModels && !s.keyInUse && s.status !== "canceled") {
    keyChoice = s.ownKey
      ? "Your own OpenRouter key is in use, not the plan's allowance."
      : "The plan's OpenRouter key isn't set up on this computer.";
  }
  const offline = s.online ? null : "Offline: this is what this computer last knew.";
  return {
    title: planName(s.plan),
    account: s.email,
    price: priceLabel(s.price),
    status,
    pitch: null,
    note,
    usage,
    warning,
    keyChoice,
    offline,
  };
}

/** Signed in to a free account: no plan bought (or none this computer knows of). */
export function isFree(s: PlanStatus): boolean {
  return s.signedIn && (s.plan === "free" || s.plan === null) && s.licence === "none";
}

/** Settings › Activity › About: the plan this computer counts, from the licence alone. */
export function aboutPlan(s: PlanStatus | null): string {
  return planName(s?.effective);
}

/** OpenRouter's 402 on a key at its monthly limit (`limit_source: "openrouter_key_limit"`), as the chat shows it. */
export function isAllowanceError(text: string): boolean {
  return /OpenRouter account is out of credits|openrouter_key_limit|key limit exceeded/i.test(text);
}

/** The chat's words when the plan's key is at its monthly limit. */
export function allowanceText(resetsAt: string): string {
  return `Your Marv plan's model allowance for this month is used up. It resets on ${shortDate(resetsAt)}. Until then, add your own OpenRouter key in Settings, or pick a local model.`;
}

/** The model menu while the Marv plan's key is in use: its OpenRouter models narrowed to the plan's (the server's
 * guardrail refuses any other, so offering one would only fail). Local models and other plans' stay. Unchanged when
 * the plan's key isn't in use or the server sent no list (`menuModels`, decided by plan.rs, offline too). */
export function planModels<M extends { id: string; local?: boolean; plan?: string }>(models: M[], s: PlanStatus | null): M[] {
  const allowed = s?.menuModels ?? [];
  if (allowed.length === 0) return models;
  return models.filter((m) => m.local || m.plan || m.id.startsWith("ollama/") || allowed.includes(m.id));
}
