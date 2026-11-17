// `npm test`: plain Node (node:test), the TypeScript stripped.
import assert from "node:assert/strict";
import { test } from "node:test";
import { reportsText } from "./format.ts";

test("Settings › About counts the hang reports", () => {
  assert.equal(reportsText(0), "No hang reports");
  assert.equal(reportsText(1), "1 report");
  assert.equal(reportsText(3), "3 reports");
});
