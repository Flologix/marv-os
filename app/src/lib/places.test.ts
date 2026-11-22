// `npm test`: plain Node (node:test), the TypeScript stripped.
import assert from "node:assert/strict";
import { test } from "node:test";
import { type BudgetView, type ChatGrant, type ChatRecord, type LiveChat, type WriteMark, chatTone, dockFor, goneFolderChats, greeting, groupChats, isHomeChat, isPlace, markFor, needsYou, relUnder, samePath, turnEnded } from "./places.ts";

const chat = (id: string, folder: string, created = 1, title = id): ChatRecord => ({ id, folder, title, created });
const live = (id: string, more: Partial<LiveChat> = {}): LiveChat => ({ id, busy: false, waiting: 0, asleep: false, cantWake: null, paused: false, ...more });

test("chats are grouped by folder, the top folder first, outside chats left out, the selected folder kept", () => {
  const chats = [chat("a", "/w/acme", 1), chat("b", "/w/acme", 5), chat("c", "/w"), chat("d", "/elsewhere"), chat("e", "/w/notes")];
  const groups = groupChats(chats, "/w", "w", "research");
  assert.deepEqual(
    groups.map((g) => [g.rel, g.name, g.chats.map((c) => c.id)]),
    [
      ["", "w", ["c"]],
      ["acme", "acme", ["b", "a"]],
      ["notes", "notes", ["e"]],
      ["research", "research", []],
    ],
  );
  // "/w/acmeX" isn't under "/w/acme": a prefix of the name isn't the folder.
  assert.deepEqual(groupChats([chat("x", "/w2/a")], "/w", "w", "").map((g) => g.chats.length), [0]);
});

// Seen on Windows (2026-11-13): chats there are in `C:\\Users\\me\\marv\\acme`, and `${root}/` never matched them, so
// no chat showed in its folder and Recent chats opened nothing.
test("Windows paths: a chat in `C:\\…\\acme` is in folder acme, whichever slashes and case", () => {
  const groups = groupChats([chat("a", "C:\\Users\\Me\\marv\\acme\\api"), chat("b", "C:\\Users\\Me\\marv")], "C:\\Users\\Me\\marv", "marv", "");
  assert.deepEqual(groups.map((g) => [g.rel, g.chats.map((c) => c.id)]), [["", ["b"]], ["acme/api", ["a"]]]);
  assert.equal(relUnder("c:/users/me/marv/Acme", "C:\\Users\\Me\\marv\\"), "Acme");
  assert.equal(relUnder("C:\\Users\\Me\\marvel", "C:\\Users\\Me\\marv"), null);
  assert.ok(samePath("C:\\Users\\Me", "c:/users/me/"));
  assert.ok(isHomeChat(chat("h", "C:/Users/Me"), "C:\\Users\\Me"));
  // Elsewhere case matters, as it does on the disk.
  assert.equal(relUnder("/W/acme", "/w"), null);
  assert.equal(relUnder("/w/acme/", "/w"), "acme");
});

test("a chat's state: waiting beats paused beats working beats asleep", () => {
  assert.equal(chatTone(undefined).tone, "off");
  assert.equal(chatTone(live("a", { waiting: 1, busy: true, paused: true })).word, "needs you");
  assert.equal(chatTone(live("a", { busy: true, paused: true })).tone, "paused");
  assert.equal(chatTone(live("a", { busy: true })).tone, "working");
  assert.equal(chatTone(live("a", { asleep: true, cantWake: "gone" })).word, "can't wake");
  assert.equal(chatTone(live("a")).tone, "idle");
});

test("needs you: waiting chats, paused or held grants, reached budgets", () => {
  const grant = (id: number, more: Partial<ChatGrant>): ChatGrant => ({ id, chat: "g", title: "nightly deps", capUsd: 1, paused: false, held: false, ...more });
  const budget: BudgetView = { scope: "global", target: "", name: "everything", capUsd: 5, reached: true };
  const needs = needsYou([live("a", { waiting: 2 }), live("b")], [chat("a", "/w", 1, "docs pass")], [grant(1, { paused: true }), grant(2, { held: true }), grant(3, {})], [budget, { ...budget, reached: false, target: "x" }]);
  assert.deepEqual(
    needs.map((n) => [n.title, n.what, n.chat ?? n.place]),
    [
      ["docs pass", "has 2 questions waiting", "a"],
      ["nightly deps", "paused at its $1.00 grant cap", "teams"],
      ["nightly deps", "is paused: mail and timers wait", "teams"],
      ["budget · everything", "reached $5.00 today: chats ask before going on", "settings"],
    ],
  );
});

test("a file is marked by its writers, a folder by any file under it", () => {
  const mark = (path: string, by: string, mine = false): WriteMark => ({ path, by, chat: null, mine, atMs: 1 });
  const marks = [mark("src/auth/redirect.rs", "fix login", true), mark("billing/x.rs", "claude-code"), mark("billing/y.rs", "docs pass")];
  assert.deepEqual(markFor("src/auth/redirect.rs", false, marks), { by: "this chat", mine: true });
  assert.deepEqual(markFor("src", true, marks), { by: "this chat", mine: true });
  assert.deepEqual(markFor("billing", true, marks), { by: "claude-code, docs pass", mine: false });
  assert.equal(markFor("bill", true, marks), null, "a name prefix isn't a parent folder");
  assert.equal(markFor("src/auth", false, marks), null);
});

test("places, greetings and the dock's remembered state", () => {
  assert.ok(isPlace("teams") && isPlace("settings") && !isPlace("mesh"));
  assert.equal(greeting(9), "Good morning");
  assert.equal(greeting(14), "Good afternoon");
  assert.equal(greeting(21), "Good evening");
  const last = { open: true, tab: "files" as const };
  assert.deepEqual(dockFor({ c1: { open: false, tab: "board" } }, "c1", last), { open: false, tab: "board" });
  assert.deepEqual(dockFor({}, "c2", last), last);
  assert.deepEqual(dockFor({ c3: { open: true, tab: "nope" as never } }, "c3", last), last);
});

// Seen 2026-11-17: a chat whose folder was renamed away had no row in the Folders tree, so no Delete anywhere.
test("chats whose folder is gone show in the nearest listed folder above it, and only once it is listed", () => {
  const chats = [chat("a", "/w/acme"), chat("g", "/w/gone"), chat("n", "/w/acme/old/deep"), chat("t", "/w")];
  const groups = groupChats(chats, "/w", "w", "");
  const top = [{ name: "acme", dir: true }, { name: "gone", dir: false }];
  assert.deepEqual(goneFolderChats(groups, "", top).map((c) => c.id), ["g"], "a file named gone isn't its folder");
  assert.deepEqual(goneFolderChats(groups, "acme", [{ name: "new", dir: true }]).map((c) => c.id), ["n"]);
  assert.deepEqual(goneFolderChats(groups, "acme", [{ name: "old", dir: true }]), []);
  assert.deepEqual(goneFolderChats(groups, "acme", undefined), [], "not listed yet: nothing is known gone");
});

test("a turn has ended only when a busy chat stops being busy", () => {
  assert.equal(turnEnded(true, false), true);
  assert.equal(turnEnded(false, false), false);
  assert.equal(turnEnded(false, true), false);
  assert.equal(turnEnded(undefined, false), false, "the first look isn't a turn's end");
  assert.equal(turnEnded(true, undefined), false, "a chat that's gone from the list isn't either");
});
