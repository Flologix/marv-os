// `npm test`: plain Node (node:test), the TypeScript stripped.
import assert from "node:assert/strict";
import { test } from "node:test";
import { harnessTitle, rememberAgentNames } from "./harness.ts";

// Seen 2026-11-17: your own agent showed as "own-hello" in the chat list, the switch dialog and the ended bar.
test("an agent is shown by its name: the known ones', and yours once a list of agents has come back", () => {
  assert.equal(harnessTitle(null), "Marv");
  assert.equal(harnessTitle("claude-code"), "Claude Code");
  assert.equal(harnessTitle("own-hello"), "own-hello", "not listed yet: its id is all there is");
  rememberAgentNames([{ id: "own-hello", name: "Hello" }, { id: "aider", name: "Aider" }, { id: "own-blank", name: "" }]);
  assert.equal(harnessTitle("own-hello"), "Hello");
  assert.equal(harnessTitle("aider"), "Aider");
  assert.equal(harnessTitle("own-blank"), "own-blank");
});
