// `npm test`: plain Node (node:test), the TypeScript stripped. mk-60m.11: the dock's Terminal tab.
import assert from "node:assert/strict";
import { test } from "node:test";
import { type ChatProcess, type ChildProcess, isProcessUrl, replyGuard, termRows } from "./places.ts";

const tool = (name: string, more: Partial<ChatProcess> = {}): ChatProcess => ({
  name,
  command: `run ${name}`,
  state: "running",
  exit_code: null,
  urls: [],
  network: false,
  notes: [],
  started_at: 1,
  ...more,
});
const proc = (name: string, pid: number, more: Partial<ChildProcess> = {}): ChildProcess => ({ kind: "process", name, pid, ...more });

test("a tool process is joined to its live row by name; an MCP server never is", () => {
  const web = tool("web", { urls: ["http://localhost:5173"] });
  const rows = termRows([proc("web", 10, { cpu_pct: 3 }), proc("web", 11, { kind: "mcp" })], [web]);
  assert.deepEqual(
    rows.map((r) => [r.key, r.name, r.pid, r.running, r.tool?.urls]),
    [
      ["tool:web", "web", 10, true, ["http://localhost:5173"]],
      ["mcp:11", "web", 11, true, undefined],
    ],
  );
});

test("a tool process Activity doesn't list still shows, with its state", () => {
  const rows = termRows([], [tool("build", { state: "exited", exit_code: 2 })]);
  assert.deepEqual(rows.map((r) => [r.key, r.running, r.pid, r.tool?.exit_code]), [["tool:build", false, undefined, 2]]);
});

test("a process the tool says exited isn't running, whatever Activity's last look said", () => {
  assert.equal(termRows([proc("w", 1)], [tool("w", { state: "exited" })])[0].running, false);
  assert.equal(termRows([proc("w", 1, { alive: false })], [])[0].running, false);
});

test("only a forwarded localhost page opens from the dock", () => {
  assert.ok(isProcessUrl("http://localhost:5173"));
  assert.ok(isProcessUrl("http://localhost:4789/app"));
  assert.ok(!isProcessUrl("https://example.com"));
  assert.ok(!isProcessUrl("http://localhost.evil.com:80"));
  assert.ok(!isProcessUrl("http://localhost:80@evil.com"));
  assert.ok(!isProcessUrl("javascript:alert(1)"));
});

test("a tool process keeps its key when Activity catches up, so its log tail stays mounted", () => {
  const before = termRows([], [tool("web")]);
  const after = termRows([proc("web", 42)], [tool("web")]);
  assert.equal(before[0].key, "tool:web");
  assert.equal(after[0].key, "tool:web");
});

test("only the latest look's replies apply, and none after the chat switched", () => {
  const guard = replyGuard();
  const first = guard.begin();
  assert.ok(first());
  const second = guard.begin();
  assert.ok(!first(), "an older look's slow reply doesn't overwrite a newer one");
  assert.ok(second());
  guard.end();
  assert.ok(!second(), "a reply about the chat you left never lands under the next one");
});
