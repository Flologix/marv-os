// `npm test`: plain Node (node:test), the TypeScript stripped.
import assert from "node:assert/strict";
import { test } from "node:test";
import { isTransportError, shouldSay } from "./errors.ts";

test("an error from a reload or a dead web process is never a dialog; the app's own errors are", () => {
  for (const broken of [
    new TypeError("Failed to fetch"),
    new TypeError("Load failed"),
    "TypeError: NetworkError when attempting to fetch resource.",
    "IPC custom protocol failed, Tauri will now use the postMessage interface instead",
    "http://ipc.localhost/list_chats: aborted",
  ]) {
    assert.ok(isTransportError(broken), String(broken));
    assert.equal(shouldSay(broken, false), false);
  }
  for (const real of ["Pick your Marv folder first.", new Error("That chat is gone."), "Couldn't open /x: No such file"]) {
    assert.ok(shouldSay(real, false), String(real));
    assert.equal(shouldSay(real, true), false, "never while the page unloads");
  }
});
