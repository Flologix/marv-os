// `npm test`: plain Node (node:test), the TypeScript stripped.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type RoleShape,
  addMember,
  boundsSummary,
  changeRole,
  chatName,
  draftProblem,
  lastReport,
  membersFor,
  parseWrites,
  processUrl,
  replyAfterLastUser,
  roleDoes,
  roleLabel,
  roster,
  scratchTemplate,
  teamSteps,
  specOf,
  splitByTeam,
  teamHeader,
  teamLead,
  missingWrites,
  takesWrites,
  writesHint,
} from "./teams.ts";
import { isProcessUrl } from "./places.ts";

const role = (name: string, more: Partial<RoleShape> = {}): RoleShape => ({
  name,
  description: name,
  tools: null,
  except: [],
  writes: null,
  takesTasksFrom: ["coordinator"],
  ...more,
});
const ROLES = [
  role("coordinator", { except: ["edit_file", "write_file", "apply_patch"], writes: [], takesTasksFrom: [] }),
  role("frontend", { writes: ["styles.css", "pages"] }),
  role("backend"),
  role("devserver", { tools: ["bash", "process", "read_file"] }),
  role("reviewer", { tools: ["read_file", "grep"], writes: [] }),
  role("worker"),
];
const r = (name: string) => ROLES.find((x) => x.name === name)!;

test("a member's bounds line says what it writes, whether it runs dev servers, and whose tasks it takes", () => {
  assert.equal(boundsSummary(r("frontend")), "writes only styles.css, pages · can run dev servers · takes tasks from coordinator");
  assert.equal(boundsSummary(r("frontend"), "styles.css, script.js pages/"), "writes only styles.css, script.js, pages/ · can run dev servers · takes tasks from coordinator");
  assert.equal(boundsSummary(r("coordinator")), "changes no files · can run dev servers · takes tasks only from you");
  assert.equal(boundsSummary(r("coordinator"), "src"), "changes no files · can run dev servers · takes tasks only from you", "typed paths can't widen []");
  assert.equal(boundsSummary(r("backend")), "no write paths: edits on a teammate's task wait for your approval · can run dev servers · takes tasks from coordinator");
  assert.equal(boundsSummary(r("reviewer")), "changes no files · takes tasks from coordinator");
  assert.equal(boundsSummary(role("x", { except: ["process"] })), "no write paths: edits on a teammate's task wait for your approval · takes tasks from coordinator");
});

test("typed write paths", () => {
  assert.deepEqual(parseWrites(" a.css,  ./pages/ src\n"), ["a.css", "./pages/", "src"]);
  assert.ok(takesWrites(r("frontend")) && takesWrites(r("backend")));
  assert.ok(!takesWrites(r("coordinator")) && !takesWrites(r("reviewer")));
  const drafts = [
    { role: "backend", title: "b", model: "", writes: " " },
    { role: "frontend", title: "f", model: "", writes: "" },
    { role: "worker", title: "w", model: "", writes: "src" },
    { role: "coordinator", title: "c", model: "", writes: "" },
  ];
  assert.deepEqual(missingWrites(drafts, ROLES).map((d) => d.title), ["b"], "frontend has its role's paths; the coordinator writes nothing");
  assert.match(writesHint("frontend"), /index\.html/);
});

test("chat names come from the folder and stay unique", () => {
  assert.equal(chatName("My Site!", "frontend", []), "my-site-frontend");
  assert.equal(chatName("Demo", "worker", ["demo-worker", "demo-worker-2"]), "demo-worker-3");
  assert.equal(chatName("", "worker", []), "team-worker");
});

test("a template's members, adding one and changing a role", () => {
  const web = { name: "web", members: [{ role: "coordinator", model: null }, { role: "frontend", model: "inherit" }, { role: "devserver", model: "small" }], worktrees: false, readChats: true };
  const drafts = membersFor(web, "Demo", ROLES);
  assert.deepEqual(drafts, [
    { role: "coordinator", title: "demo-coordinator", model: "", writes: "" },
    { role: "frontend", title: "demo-frontend", model: "inherit", writes: "styles.css, pages" },
    { role: "devserver", title: "demo-devserver", model: "small", writes: "" },
  ]);
  const more = addMember(drafts, "Demo", ROLES);
  assert.deepEqual(more[3], { role: "worker", title: "demo-worker", model: "", writes: "" });
  const changed = changeRole(more, 3, "frontend", "Demo", ROLES);
  assert.deepEqual(changed[3], { role: "frontend", title: "demo-frontend-2", model: "", writes: "styles.css, pages" });
  const renamed = changeRole([{ ...more[3], title: "ui" }], 0, "backend", "Demo", ROLES);
  assert.equal(renamed[0].title, "ui", "a name the person typed stays");
});

test("the spec sent to the backend, and what stops a start", () => {
  const drafts = [
    { role: "coordinator", title: " demo-coordinator ", model: "", writes: "" },
    { role: "frontend", title: "demo-frontend", model: "small", writes: "a.css, pages/" },
  ];
  const spec = specOf(" web ", drafts, { worktrees: false, readChats: true, keepWorking: false }, { model: "z/glm", maxSteps: 50, yolo: true });
  assert.equal(spec.name, "web");
  assert.deepEqual(spec.members, [
    { role: "coordinator", title: "demo-coordinator", model: null, writes: null },
    { role: "frontend", title: "demo-frontend", model: "small", writes: ["a.css", "pages/"] },
  ]);
  assert.equal(spec.defaultModel, "z/glm");
  assert.equal(draftProblem("web", drafts), null);
  assert.match(draftProblem("Web App", drafts)!, /lowercase/);
  assert.match(draftProblem("web", [])!, /Add a member/);
  assert.match(draftProblem("web", [drafts[1], drafts[1]])!, /same name/);
  assert.match(draftProblem("web", [{ ...drafts[1], title: " " }])!, /needs a name/);
});

test("only a localhost port's own page opens from a process", () => {
  assert.equal(processUrl("vite :5173"), "http://localhost:5173");
  assert.equal(processUrl("dev localhost:3000/app"), "http://localhost:3000");
  assert.equal(processUrl("npm run dev"), null);
  assert.equal(processUrl("evil.com:80"), "http://localhost:80", "a host in the name is never opened, only localhost");
  assert.equal(processUrl("port :99999"), null);
  assert.ok(isProcessUrl("http://localhost:5173"));
  assert.ok(!isProcessUrl("http://localhost.evil.com:80"));
  assert.ok(!isProcessUrl("http://localhost:80@evil.com"));
  assert.ok(!isProcessUrl("javascript:alert(1)"));
});

test("the roster: a member its own Marv says waits or works shows so, as Activity does, beside a live state of idle", () => {
  const chats = [
    { id: "aaaaaaaa1", title: "lead", folder: "/p", role: "coordinator" },
    { id: "bbbbbbbb2", title: "worker", folder: "/p", role: "builder" },
    { id: "cccccccc3", title: "worker-2", folder: "/p", role: "builder" },
  ];
  const idle = (id: string) => ({ id, busy: false, waiting: 0, asleep: false, cantWake: null, paused: false });
  const agents = [
    { name: "p-aaaaaaaa", live: {}, state: "busy" as const },
    { chat: "bbbbbbbb2", name: "p-bbbbbbbb", live: {}, state: "waiting" as const },
    { chat: "cccccccc3", name: "p-cccccccc", live: {}, state: "idle" as const },
  ];
  const cards = roster(chats.map((c) => c.id), chats, chats.map((c) => idle(c.id)), agents, [], []);
  assert.deepEqual(cards.map((c) => [c.tone, c.word, c.waiting]), [["working", "working", 0], ["waiting", "needs you", 1], ["idle", "idle", 0]]);
});

test("the roster: each member's state, model, overlaps, processes and last report, in start order", () => {
  const chats = [
    { id: "b", title: "demo-frontend", folder: "/p", role: "frontend", settings: { model: "z/glm" }, writes: ["index.html"] },
    { id: "a", title: "demo-coordinator", folder: "/p", role: "coordinator", settings: { model: "z/big" } },
    { id: "x", title: "other", folder: "/p" },
  ];
  const live = [
    { id: "a", busy: true, waiting: 0, asleep: false, cantWake: null, paused: false },
    { id: "b", busy: false, waiting: 2, asleep: false, cantWake: null, paused: false },
  ];
  const agents = [{ chat: "b", live: { core: "openrouter/z/glm", processes: [{ kind: "process", name: "vite :5173" }, { kind: "mcp", name: "fs" }, { kind: "process", name: "old", alive: false }] } }];
  const overlaps = [{ chat: "b", with: [{ name: "demo-backend", paths: ["styles.css"] }] }];
  const replies = [{ chat: "a", text: "Split into 3 tasks." }];
  const cards = roster(["a", "b", "gone"], chats, live, agents, overlaps, replies);
  assert.deepEqual(
    cards.map((c) => [c.id, c.role, c.word, c.model, c.waiting, c.editing, c.processes, c.report]),
    [
      ["a", "coordinator", "working", "z/big", 0, [], [], "Split into 3 tasks."],
      ["b", "frontend", "needs you", "openrouter/z/glm", 2, ["styles.css (also: demo-backend)"], [{ name: "vite :5173", url: "http://localhost:5173" }], null],
    ],
  );
  assert.deepEqual(cards.map((c) => c.writes), [null, ["index.html"]]);
  assert.equal(lastReport({ id: "a" }, replies), "Split into 3 tasks.");
  assert.equal(teamLead(["b", "a"], chats), "a");
  assert.equal(teamLead(["b"], chats), "b");
  assert.equal(teamLead([], chats), null);
});

test("the sidebar groups a team's chats under its header, in roster order", () => {
  const team = { name: "web", roster: ["coordinator", "frontend", "backend"] };
  const chats = [
    { id: "1", title: "demo-backend", folder: "/p", role: "backend", team },
    { id: "2", title: "notes", folder: "/p" },
    { id: "3", title: "demo-coordinator", folder: "/p", role: "coordinator", team },
    { id: "4", title: "demo-frontend", folder: "/p", role: "frontend", team },
  ];
  const { teams, rest } = splitByTeam(chats);
  assert.deepEqual(teams.map((t) => [t.name, t.chats.map((c) => c.id)]), [["web", ["3", "4", "1"]]]);
  assert.deepEqual(rest.map((c) => c.id), ["2"]);
  assert.equal(teamHeader("web", teams[0].chats), "Team web: coordinator, frontend, backend");
});

test("Tell the team shows the lead's reply to your last message", () => {
  assert.equal(replyAfterLastUser([]), null);
  assert.equal(replyAfterLastUser([{ kind: "user", text: "go" }]), null);
  assert.equal(
    replyAfterLastUser([{ kind: "text", text: "old" }, { kind: "user", text: "go" }, { kind: "tool" }, { kind: "text", text: " Sent 3 tasks. " }, { kind: "text", text: "Waiting." }]),
    "Sent 3 tasks.\n\nWaiting.",
  );
});

test("the dialog names roles in plain words, a folder's own role as its file says", () => {
  assert.equal(roleLabel("coordinator"), "Lead");
  assert.equal(roleLabel("designer"), "Designer");
  assert.match(roleDoes(r("coordinator")), /hands tasks/);
  assert.equal(roleDoes(role("designer", { description: "Draws the screens." })), "Draws the screens.");
});

test("from scratch is a lead and a builder, or the first role a folder has", () => {
  assert.deepEqual(scratchTemplate(ROLES).members.map((m) => m.role), ["coordinator", "worker"]);
  assert.deepEqual(scratchTemplate([role("designer")]).members.map((m) => m.role), ["designer"]);
  assert.equal(draftProblem(scratchTemplate(ROLES).name, membersFor(scratchTemplate(ROLES), "shop", ROLES)), null);
});

test("a team member gets at least 300 steps, never past 500", () => {
  assert.equal(teamSteps(50), 300);
  assert.equal(teamSteps(400), 400);
  assert.equal(teamSteps(900), 500);
  assert.equal(specOf("web", [], { worktrees: false, readChats: true, keepWorking: false }, { model: "m", maxSteps: 100, yolo: true }).maxSteps, 300);
});
