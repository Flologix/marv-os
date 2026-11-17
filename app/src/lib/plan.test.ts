// `npm test`: plain Node (node:test), the TypeScript stripped.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  FREE_NOTE,
  type PlanStatus,
  aboutPlan,
  allowancePercent,
  allowanceText,
  isAllowanceError,
  isFree,
  nextReset,
  planModels,
  planName,
  planView,
  priceLabel,
} from "./plan.ts";

const SIGNED_OUT: PlanStatus = {
  signedIn: false,
  email: null,
  plan: null,
  status: null,
  periodEnd: null,
  price: null,
  licence: "none",
  licenceExpires: null,
  effective: "free",
  allowance: null,
  hasModels: false,
  keyInUse: false,
  menuModels: [],
  ownKey: false,
  online: false,
  notice: null,
  marketing: null,
};

const PRO: PlanStatus = {
  ...SIGNED_OUT,
  signedIn: true,
  email: "a@b.co",
  plan: "pro_models",
  status: "active",
  licence: "valid",
  effective: "pro_models",
  allowance: { limitUsd: 8, usedUsd: 1.25, resetsAt: "2026-12-01T00:00:00Z", defaultModel: "z-ai/glm-5.3", allowedModels: [] },
  hasModels: true,
  keyInUse: true,
  menuModels: [],
  online: true,
  price: { amount: 20, currency: "usd", interval: "month" },
};

test("signed out: what Pro + models gives, nothing else", () => {
  const v = planView(SIGNED_OUT);
  assert.equal(v.title, "Marv account");
  assert.equal(v.account, null);
  assert.deepEqual(v.status, { text: "Not signed in", tone: "off" });
  assert.equal(v.pitch, "Pro + models gives you fast coding models, DeepSeek V4.1 Flash first, with a monthly allowance and no key of your own to set up.");
  assert.equal(v.usage, null);
  assert.equal(v.warning, null);
  assert.equal(v.keyChoice, null);
  assert.equal(v.offline, null);
  assert.equal(aboutPlan(SIGNED_OUT), "Free");
  assert.equal(aboutPlan(null), "Free");
});

test("a free account: signed in, the email, what upgrading adds, no warning about a licence it never had", () => {
  const free: PlanStatus = { ...SIGNED_OUT, signedIn: true, email: "a@b.co", plan: "free", status: "active", online: true };
  assert.ok(isFree(free));
  const v = planView(free);
  assert.equal(v.title, "Free");
  assert.equal(v.account, "a@b.co");
  assert.equal(v.note, FREE_NOTE);
  assert.equal(v.warning, null);
  assert.equal(v.status, null);
  assert.equal(planView({ ...free, online: false }).offline, "Offline: this is what this computer last knew.");
  assert.ok(!isFree(PRO));
  assert.ok(!isFree(SIGNED_OUT));
});

test("Pro + models: the plan, its status and the share of the allowance used, dollars only as detail", () => {
  const v = planView(PRO);
  assert.equal(v.title, "Pro + models");
  assert.equal(v.account, "a@b.co");
  assert.deepEqual(v.status, { text: "Active", tone: "ok" });
  assert.equal(v.pitch, null);
  assert.deepEqual(v.usage, {
    percent: 16,
    text: "16% of this month's allowance used · resets Dec 1",
    detail: "$1.25 of $8.00 used · $6.75 left",
  });
  assert.equal(v.warning, null);
  assert.equal(v.keyChoice, null);
  assert.equal(v.offline, null);
  assert.equal(aboutPlan(PRO), "Pro + models");
  const noReset = planView({ ...PRO, allowance: { ...PRO.allowance!, resetsAt: null } });
  assert.equal(noReset.usage?.text, "16% of this month's allowance used");
});

test("the allowance percent: rounded, clamped to 0..100, none without a limit", () => {
  assert.equal(allowancePercent(1.25, 8), 16);
  assert.equal(allowancePercent(0, 8), 0);
  assert.equal(allowancePercent(8, 8), 100);
  assert.equal(allowancePercent(9.5, 8), 100, "over the limit: full, not 119%");
  assert.equal(allowancePercent(-1, 8), 0);
  assert.equal(allowancePercent(0.03, 8), 0);
  assert.equal(allowancePercent(0.04, 8), 1);
  assert.equal(allowancePercent(null, 8), 0);
  assert.equal(allowancePercent(1, 0), null);
  assert.equal(allowancePercent(1, null), null);
  assert.equal(allowancePercent(1, undefined), null);
  assert.equal(allowancePercent(1, Number.NaN), null);
  assert.equal(planView({ ...PRO, allowance: { ...PRO.allowance!, limitUsd: 0 } }).usage, null);
});

test("Founding Member: no allowance, a note instead; offline shows no status pill", () => {
  const founding: PlanStatus = { ...PRO, plan: "founding", effective: "founding", allowance: null, hasModels: false, keyInUse: false };
  const v = planView(founding);
  assert.equal(v.title, "Founding Member");
  assert.deepEqual(v.status, { text: "Active", tone: "ok" });
  assert.equal(v.usage, null);
  assert.match(v.note ?? "", /models aren't included/);
  const offline = planView({ ...founding, status: null, online: false });
  assert.equal(offline.status, null);
  assert.equal(offline.offline, "Offline: this is what this computer last knew.");
  assert.equal(aboutPlan(founding), "Founding Member");
  assert.equal(planView(PRO).note, null);
});

test("the person's own key in use, or none: the plan's key is offered", () => {
  assert.equal(planView({ ...PRO, keyInUse: false, ownKey: true }).keyChoice, "Your own OpenRouter key is in use, not the plan's allowance.");
  assert.equal(planView({ ...PRO, keyInUse: false }).keyChoice, "The plan's OpenRouter key isn't set up on this computer.");
  assert.equal(planView({ ...PRO, keyInUse: false, ownKey: true, status: "canceled" }).keyChoice, null);
  assert.equal(planView({ ...PRO, keyInUse: false, online: false, status: null }).keyChoice, null, "offline: not claimed either way");
});

test("payment due, an expired or unconfirmed licence: said plainly, nothing locked", () => {
  assert.match(planView({ ...PRO, status: "past_due" }).warning ?? "", /payment didn't go through/);
  assert.deepEqual(planView({ ...PRO, status: "past_due" }).status, { text: "Payment due", tone: "warn" });
  assert.deepEqual(planView({ ...PRO, status: "canceled" }).status, { text: "Canceled", tone: "off" });
  const expired = { ...PRO, effective: "free", licence: "expired" as const };
  assert.equal(planView(expired).warning, "The plan's licence has expired, so this computer counts it as Free.");
  assert.equal(aboutPlan(expired), "Free");
  assert.match(planView({ ...PRO, effective: "free", licence: "invalid" }).warning ?? "", /couldn't confirm/);
});

test("names, the monthly reset and the allowance error", () => {
  assert.equal(planName("founding"), "Founding Member");
  assert.equal(planName("pro_models"), "Pro + models");
  assert.equal(planName("other"), "Free");
  assert.equal(nextReset(new Date("2026-11-30T23:59:00Z")), "2026-12-01T00:00:00.000Z");
  assert.equal(nextReset(new Date("2026-12-15T00:00:00Z")), "2027-01-01T00:00:00.000Z");
  assert.ok(isAllowanceError("Your OpenRouter account is out of credits.\nKey limit exceeded"));
  assert.ok(!isAllowanceError("Rate limited by OpenRouter. Wait a moment and try again."));
  assert.equal(
    allowanceText("2026-12-01T00:00:00Z"),
    "Your Marv plan's model allowance for this month is used up. It resets on Dec 1. Until then, add your own OpenRouter key in Settings, or pick a local model.",
  );
});

test("the plan's price: shown from the server, never made up", () => {
  assert.equal(planView(PRO).price, "$20/month");
  assert.equal(planView(SIGNED_OUT).price, null);
  assert.equal(planView({ ...PRO, price: null, online: false }).price, null, "offline: no price");
  assert.equal(priceLabel({ amount: 96, currency: "usd", interval: "year" }), "$96/year");
  assert.equal(priceLabel({ amount: 9.5, currency: "usd", interval: "month" }), "$9.50/month");
  assert.equal(priceLabel({ amount: 20, currency: "eur", interval: "month" }), "€20/month");
  assert.equal(priceLabel({ amount: 20, currency: "not a currency", interval: "month" }), null);
  assert.equal(priceLabel({ amount: 20, currency: "usd", interval: "week" }), null);
  assert.equal(priceLabel({ amount: Number.NaN, currency: "usd", interval: "month" }), null);
});

// On the Marv plan's key, the model menu offers only the plan's models (the server's guardrail refuses others);
// local models and other plans' stay, and nothing is narrowed off the plan's key or without a list.
test("the model menu narrows to the plan's models only while its key is in use", () => {
  const models = [
    { id: "deepseek/deepseek-v4.1-flash" },
    { id: "anthropic/claude-sonnet-4.5" },
    { id: "ollama/qwen3:14b", local: true },
    { id: "zai/glm-5.3", plan: "Z.ai Coding Plan" },
  ];
  // plan.rs decides `menuModels` (the plan's list only while its key is in use).
  const status = (keyInUse: boolean, allowedModels: string[]) => ({ keyInUse, menuModels: keyInUse ? allowedModels : [] }) as unknown as PlanStatus;
  assert.deepEqual(
    planModels(models, status(true, ["deepseek/deepseek-v4.1-flash"])).map((m) => m.id),
    ["deepseek/deepseek-v4.1-flash", "ollama/qwen3:14b", "zai/glm-5.3"],
  );
  assert.equal(planModels(models, status(false, ["deepseek/deepseek-v4.1-flash"])).length, 4);
  assert.equal(planModels(models, status(true, [])).length, 4);
  assert.equal(planModels(models, null).length, 4);
});
