// A stand-in for Marv desktop's Tauri backend, for the README screenshot of app/ in headless Chromium.
// capture-desktop.sh injects it before the app's own script: the page then believes it runs in Tauri, and every
// command answers with the demo data below (a Marv folder with a few projects, three chats, one waiting for an
// approval). Nothing runs: no model, no files, no network.
(() => {
  const root = "/home/demo/Projects";
  const space = { id: "s1", name: "Projects", root, appsOff: [] };
  const info = { root, rootName: "Projects", hasKey: true, provider: "openrouter", ready: true,
    offerZai: false, offerKimi: false, offerChatgpt: false, hasZaiKey: false, hasKimiKey: false, chatgptAccount: null, spaces: [space], active: "s1" };
  const now = Date.now() / 1000; // chats' times are in seconds
  const chats = [
    {
      id: "c1", folder: `${root}/todo-api`, title: "Add a /health endpoint", created: now - 4 * 60,
      worktree: { dir: `${root}/.worktrees/health`, branch: "marv/health-3fa2", base: "a1b2c3d", top: "" },
    },
    { id: "c2", folder: `${root}/todo-api`, title: "Why is the 404 test slow?", created: now - 26 * 3600 },
    { id: "c3", folder: `${root}/site`, title: "Fix the nav on small screens", created: now - 12 * 60 },
    { id: "c4", folder: `${root}/notes`, title: "Weekly review", created: now - 3 * 3600 },
  ];
  // Kernel #53's places: ?place=home|folders|teams|activity|apps|settings, and ?dock=files|changes|board|terminal|context.
  const params = new URLSearchParams(location.search);
  const place = params.get("place") ?? "folders";
  const dockTab = params.get("dock") ?? "files";
  // One-step team setup: ?team=new (the New team dialog open on site) or ?team=view (team web started in site).
  const teamMode = params.get("team");
  const webTeam = { name: "web", roster: ["coordinator", "frontend", "backend", "devserver"] };
  if (teamMode === "view") {
    const member = (id, role, model, minutes) => ({ id, folder: `${root}/site`, title: `site-${role}`, created: now - minutes * 60, role, team: webTeam, settings: { model, maxSteps: 50, yolo: true } });
    chats.push(member("c5", "coordinator", "z-ai/glm-5.3", 30), { ...member("c6", "frontend", "z-ai/glm-5.3-flash", 29), writes: ["index.html", "styles.css", "nav.js"] }, { ...member("c7", "backend", "z-ai/glm-5.3-flash", 29), writes: ["server", "tests"] }, member("c8", "devserver", "qwen/qwen3-8b", 28));
  }
  const roles = [
    { name: "backend", description: "Builds the back end (server, APIs, data) on the coordinator's tasks and reports back.", source: "built-in", tools: null, except: [], writes: null, takesTasksFrom: ["coordinator"] },
    { name: "coordinator", description: "Plans the work, hands tasks to worker chats over the mesh, and checks their reports. Doesn't edit files itself.", source: "built-in", tools: null, except: ["edit_file", "write_file", "apply_patch"], writes: [], takesTasksFrom: [] },
    { name: "devserver", description: "Runs the dev server, says its localhost address, and reports its errors. Doesn't change files (a server that writes a cache asks).", source: "built-in", tools: ["read_file", "glob", "grep", "bash", "process", "mesh_list", "mesh_send", "mesh_wait", "board_read", "board_post"], except: [], writes: [], takesTasksFrom: ["coordinator"] },
    { name: "frontend", description: "Builds the front end (pages, components, styles) on the coordinator's tasks and reports back.", source: "built-in", tools: null, except: [], writes: null, takesTasksFrom: ["coordinator"] },
    { name: "reviewer", description: "Reviews finished work: reads the changes, runs the checks, reports problems. Doesn't edit files.", source: "built-in", tools: null, except: ["edit_file", "write_file", "apply_patch"], writes: [], takesTasksFrom: ["coordinator", "worker"] },
    { name: "worker", description: "Does the tasks the coordinator sends within its bounds, and reports what it did with evidence.", source: "built-in", tools: null, except: [], writes: null, takesTasksFrom: ["coordinator"] },
  ];
  const templates = [
    { name: "web", title: "Web app", description: "A coordinator, a frontend and a backend chat, and one that runs the dev server.", source: "built-in", members: [{ role: "coordinator", model: null }, { role: "frontend", model: null }, { role: "backend", model: null }, { role: "devserver", model: "small" }], worktrees: false, readChats: true, brief: false, briefText: "" },
    { name: "build-review", title: "Build + review", description: "A coordinator hands tasks to a worker; a reviewer checks the work.", source: "built-in", members: [{ role: "coordinator", model: null }, { role: "worker", model: null }, { role: "reviewer", model: null }], worktrees: false, readChats: true, brief: false, briefText: "" },
    { name: "pair", title: "Pair", description: "A worker you talk to, and a reviewer that checks its work.", source: "built-in", members: [{ role: "worker", model: null }, { role: "reviewer", model: null }], worktrees: false, readChats: true, brief: false, briefText: "" },
  ];
  const ms = Date.now();
  const rule = { id: 1, label: "todo-api", from: `${root}/todo-api`, to: `${root}/todo-api`, mode: "send", mutual: true, expiresAtMs: null, untilQuit: false, by: "desktop" };
  const grants = {
    problem: null,
    grants: [
      {
        id: 7, chat: "c2", title: "Nightly deps check", folder: `${root}/todo-api`, expiresAtMs: null, untilQuit: false, spentUsd: 1, capUsd: 1,
        spentTokens: 0, capTokens: 0, lastRunMs: ms - 6 * 3600e3, paused: true, held: false,
        timers: [{ id: 3, by: "person", atMs: ms + 9 * 3600e3, everyMinutes: null, dailyAt: "09:00", note: "check the deps" }],
      },
    ],
  };
  const usage = {
    todayUsd: 1.84, weekUsd: 9.12, todayRequests: 212, problem: null,
    rows: [
      { kind: "folder", key: `${root}/todo-api`, name: "todo-api", todayUsd: 1.42, weekUsd: 6.8, todayTokens: 0, weekTokens: 0, todayRequests: 160 },
      { kind: "core", key: "z-ai/glm-5.3-flash", name: "z-ai/glm-5.3-flash", todayUsd: 1.84, weekUsd: 9.12, todayTokens: 0, weekTokens: 0, todayRequests: 212 },
    ],
    budgets: [{ scope: "global", target: "", name: "everything", capUsd: 5, spentUsd: 1.84, percent: 37, warning: false, reached: false }],
  };
  const metrics = (model, tools, you) => ({ model_pct: model, tools_pct: tools, you_pct: you, tokens_per_s: 41, usd_micros_per_hour: 420000, cache_hit_pct: 88, requests_per_min: 6, retries: 0, throttles: 0 });
  const activity = {
    teams: [{ rule: 1, label: "todo-api", names: ["todo-api-3f9a", "todo-api-81c0"] }],
    activity: {
      bar: { loaded: 2, cap: 8, spend_usd_micros: 1840000, models: [{ model: "z-ai/glm-5.3-flash", agents: 2, tokens_per_s: 41, usd_micros_per_hour: 420000, cache_hit_pct: 88, throttles: 0 }] },
      rows: [
        {
          name: "todo-api-3f9a", folder: `${root}/todo-api`, kind: "chat", chat: "c1", pid: 4120, state: "busy", title: "Add a /health endpoint",
          live: {
            core: "z-ai/glm-5.3-flash", context: { used: 42000, budget: 100000 }, metrics: metrics(61, 30, 9), paused: false,
            processes: [
              { kind: "process", name: "python3 -m http.server 8000", pid: 4188, rss_kb: 21000, cpu_pct: 0.4, alive: true },
              { kind: "process", name: "python3 -m unittest -q", pid: 4201, rss_kb: 18000, cpu_pct: 12, alive: false },
            ],
          },
        },
        { name: "todo-api-81c0", folder: `${root}/todo-api`, kind: "chat", chat: "c2", pid: 4120, state: "sleeping", title: "Why is the 404 test slow?", live: { paused: false, context: { used: 71000, budget: 100000 } } },
        { name: "site-0d2e", folder: `${root}/site`, kind: "chat", chat: "c3", pid: 4120, state: "waiting", title: "Fix the nav on small screens", live: { core: "z-ai/glm-5.3-flash", context: { used: 18000, budget: 100000 }, metrics: metrics(0, 0, 100), paused: false } },
        { name: "site-0c8e", folder: `${root}/site`, kind: "chat", chat: "c8", pid: 4120, state: "sleeping", title: "", live: { core: "qwen/qwen3-8b", paused: false, processes: [{ kind: "process", name: "vite :5173", pid: 4301, alive: true }] } },
        { name: "site-0c6f", folder: `${root}/site`, kind: "chat", chat: "c6", pid: 4120, state: "busy", title: "", live: { core: "z-ai/glm-5.3-flash", paused: false } },
        { name: "todo-api-7c1e", folder: `${root}/todo-api`, kind: "terminal", pid: 48213, state: "busy", title: "billing webhooks", live: { core: "claude-sonnet", paused: false } },
      ],
    },
  };
  const board = {
    rule: 1, problem: null,
    entries: [
      { id: 1, by: "todo-api-3f9a", kind: "claim", text: "app.py: the /health route and its test", paths: ["app.py"], atMs: ms - 12 * 60e3 },
      { id: 2, by: "todo-api-81c0", kind: "note", text: "the 404 test waits on a 2 s timeout; looking at why", paths: [], atMs: ms - 8 * 60e3 },
      { id: 3, by: "", kind: "done", text: "decided: health stays unauthenticated", paths: [], atMs: ms - 60 * 60e3 },
    ],
  };
  const timeline = [
    { at_ms: ms - 7 * 3600e3, kind: "wake", chat: "c2", agents: ["todo-api-81c0"], text: "Nightly deps check woke on its timer and ran 4 tools" },
    { at_ms: ms - 7 * 3600e3 + 4 * 60e3, kind: "paused", chat: "c2", agents: ["todo-api-81c0"], text: "Nightly deps check paused at its spend cap and told you" },
    { at_ms: ms - 90 * 60e3, kind: "finished", chat: "c1", agents: ["todo-api-3f9a"], text: "Add a /health endpoint finished a turn: 4 tests pass" },
    { at_ms: ms - 25 * 60e3, kind: "asleep", chat: "c4", agents: ["notes-55aa"], text: "Weekly review went to sleep" },
  ].reverse(); // the kernel sends newest first
  const tree = {
    "": [
      { name: "notes", dir: true },
      { name: "site", dir: true },
      { name: "todo-api", dir: true },
    ],
    "todo-api/tests": [{ name: "test_app.py", dir: false }],
    "todo-api": [
      { name: "tests", dir: true },
      { name: "AGENTS.md", dir: false },
      { name: "README.md", dir: false },
      { name: "app.py", dir: false },
    ],
  };
  const line = (kind, text) => ({ kind, text });
  const transcript = [
    { kind: "user", text: "add a /health endpoint with a test, then commit it" },
    { kind: "thinking", text: "Routes are plain ifs in route(); the tests call route() directly." },
    { kind: "text", text: "I'll look at the routes and the tests first." },
    { kind: "tool", id: "t1", tool: "read_file", label: "app.py", done: true, summary: "34 lines", result: "" },
    { kind: "tool", id: "t2", tool: "read_file", label: "tests/test_app.py", done: true, summary: "21 lines", result: "" },
    {
      kind: "tool", id: "t3", tool: "edit_file", label: "app.py", done: true, summary: "+2 −0",
      diff: {
        more: 0,
        lines: [
          line("ctx", "def route(method, path):"),
          line("add", '    if method == "GET" and path == "/health":'),
          line("add", '        return 200, {"status": "ok", "todos": len(TODOS)}'),
          line("ctx", '    if method == "GET" and path == "/todos":'),
          line("ctx", "        return 200, list(TODOS.values())"),
        ],
      },
    },
    { kind: "tool", id: "t4", tool: "edit_file", label: "tests/test_app.py", done: true, summary: "+7 −0", result: "" },
    {
      kind: "tool", id: "t5", tool: "bash", label: "python3 -m unittest -q", done: true, summary: "exit 0",
      result: "----------------------------------------------------------------------\nRan 4 tests in 0.001s\n\nOK",
    },
    { kind: "text", text: "All 4 tests pass. Committing the change." },
    { kind: "tool", id: "t6", tool: "bash", label: 'git add app.py tests/test_app.py && git commit -m "Add /health endpoint"', done: false },
    {
      kind: "approval",
      request: {
        id: 1, tool: "bash", label: "git commit", title: "Run a command",
        command: 'git add app.py tests/test_app.py && git commit -m "Add /health endpoint"',
        note: "sandboxed · no network · changes the git repository", scope: "this command", once: false, reason: true,
      },
    },
  ];
  try {
    localStorage.clear();
    localStorage.setItem("marv-folder", JSON.stringify(teamMode ? "site" : "todo-api"));
    if (teamMode === "view") localStorage.setItem("marv-transcript-c5", JSON.stringify([{ kind: "user", text: "make the nav work on small screens" }, { kind: "text", text: "Sent 3 tasks: site-frontend owns styles.css and nav.js, site-backend the /menu API, site-devserver keeps vite running. Waiting for their reports." }]));
    localStorage.setItem("marv-place", JSON.stringify(place));
    localStorage.setItem("marv-theme", params.get("theme") ?? "light");
    localStorage.setItem("marv-dock-last", JSON.stringify({ open: true, tab: dockTab }));
    localStorage.setItem("marv-transcript-c1", JSON.stringify(transcript));
    localStorage.setItem("marv-seq-c1", "40");
    localStorage.setItem("marv-epoch-c1", "demo");
    const model = { id: "z-ai/glm-5.3-flash", name: "Z.ai: GLM 5.3 Flash", context: 200000, input: 0.3, output: 1.2 };
    localStorage.setItem("marv-models-2", JSON.stringify({ at: Date.now(), models: [model] }));
  } catch {}

  const callbacks = {};
  let next = 1;
  const send = (channel, index, message) => callbacks[channel.id]?.({ index, message });
  const commands = {
    app_info: () => info,
    take_launch: () => null,
    list_dir: ({ rel }) => tree[rel] ?? [],
    list_chats: ({ folder }) => (folder === undefined ? chats : chats.filter((c) => c.folder === `${root}/${folder}`)),
    list_roles: () => [roles, []],
    list_teams: () => ({ teams: templates, problems: [], hasFile: false, meshWake: "off", roleFiles: [], here: teamMode === "view" ? [{ name: "web", rules: [2, 3], chats: ["c5", "c6", "c7", "c8"] }] : [] }),
    team_reports: () => [
      { chat: "c6", text: "waiting for its user: it asked its user to approve edit_file and waits for the answer" },
      { chat: "c7", text: "done: GET /menu returns the nav items; tests/test_menu.py passes (2 files changed)" },
      { chat: "c8", text: "done: vite is up on http://localhost:5173, no errors so far" },
    ],
    live_chats: () => [
      { id: "c5", busy: false, waiting: 0, asleep: false, cantWake: null, paused: false },
      { id: "c6", busy: true, waiting: 1, asleep: false, cantWake: null, paused: false },
      { id: "c7", busy: false, waiting: 0, asleep: false, cantWake: null, paused: false },
      { id: "c8", busy: false, waiting: 0, asleep: true, cantWake: null, paused: false },
      { id: "c1", busy: true, waiting: 1, asleep: false, cantWake: null, paused: false },
      { id: "c2", busy: false, waiting: 0, asleep: true, cantWake: null, paused: false },
      { id: "c3", busy: true, waiting: 1, asleep: false, cantWake: null, paused: false },
      { id: "c4", busy: false, waiting: 0, asleep: false, cantWake: null, paused: false },
    ],
    mcp_servers: () => [
      { name: "github", scope: "global", target: "~/.marv/mcp.json" },
      { name: "linear", scope: "global", target: "~/.marv/mcp.json" },
    ],
    chat_grants: () => grants,
    usage_view: () => usage,
    activity: () => activity,
    timeline: () => timeline,
    notifications: () => [],
    mesh_rules: () => ({ rules: teamMode === "view" ? [rule, { ...rule, id: 2, label: "Team web", from: `${root}/site`, to: `${root}/site` }, { ...rule, id: 3, label: "Team web: read each other's chats", mode: "read", from: `${root}/site`, to: `${root}/site` }] : [rule], problem: null }),
    board: () => board,
    write_overlaps: () => (teamMode === "view" ? [{ chat: "c6", title: "site-frontend", with: [{ chat: "c7", name: "site-backend", paths: ["nav.js"] }] }] : []),
    chat_worktree: ({ id }) => chats.find((c) => c.id === id)?.worktree ?? null,
    marv_config: () => ({ smallModel: null, meshWake: null, meshRadio: false, meshCards: true, problem: null }),
    chat_devices: () => ({ names: ["github", "herdr", "host", "process", "mcp:linear"], off: ["herdr"] }),
    permissions: () => [
      { chat: "c1", name: "Add a /health endpoint", may: [{ text: "message chats under todo-api (rule 1)", revoke: { kind: "rule", id: 1 } }] },
      { chat: "c2", name: "Why is the 404 test slow?", may: [{ text: "work while you're away, $1.00 a day (grant 7)", revoke: { kind: "grant", id: 7 } }] },
      { chat: "c3", name: "Fix the nav on small screens", may: [] },
    ],
    about_marv: () => ({ version: "0.9.0", limits: [], stores: [] }),
    watchdog_notice: () => null,
    hang_reports: () => ({ count: 0, dir: "~/.local/share/ai.flologix.marv/reports" }),
    open_hang_reports: () => null,
    chat_files: ({ rel }) => tree[rel ? `todo-api/${rel}` : "todo-api"] ?? [],
    chat_writes: () => [
      { path: "app.py", by: "Add a /health endpoint", chat: "c1", mine: true, atMs: ms - 60e3 },
      { path: "tests/test_app.py", by: "Add a /health endpoint", chat: "c1", mine: true, atMs: ms - 50e3 },
    ],
    chat_changes: () => ({
      against: "a1b2c3d", more_files: 0, problem: null,
      files: [
        {
          path: "app.py", status: "modified", added: 2, removed: 0, more: 0,
          lines: [
            { kind: "gap", text: "def route(method, path):" },
            { kind: "ctx", text: "def route(method, path):" },
            { kind: "add", text: '    if method == "GET" and path == "/health":' },
            { kind: "add", text: '        return 200, {"status": "ok", "todos": len(TODOS)}' },
            { kind: "ctx", text: '    if method == "GET" and path == "/todos":' },
          ],
        },
        { path: "tests/test_app.py", status: "modified", added: 7, removed: 0, more: 0, lines: [] },
      ],
    }),
    plan_models: () => [],
    open_chat: ({ onEvent }) => {
      setTimeout(() => send(onEvent, 0, { type: "ready", epoch: "demo", busy: true, mesh: "todo-api-3f9a", commands: [], contextBudget: 100000, contextBudgetSet: 100000 }), 10);
      setTimeout(() => send(onEvent, 1, { type: "context", context: 42000 }), 20);
      return null;
    },
    "plugin:event|listen": () => next++,
  };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
    transformCallback: (cb) => {
      const id = next++;
      callbacks[id] = cb;
      return id;
    },
    unregisterCallback: (id) => delete callbacks[id],
    invoke: async (cmd, args = {}) => (commands[cmd] ? commands[cmd](args) : null),
    convertFileSrc: (p) => p,
  };

  // A still picture: no fade-ins half done when the screenshot is taken.
  document.addEventListener("DOMContentLoaded", () => {
    const style = document.createElement("style");
    style.textContent = "*, *::before, *::after { animation: none !important; transition: none !important; }";
    document.head.append(style);
  });

  // ?team=new: the New team dialog, opened as a person would.
  if (teamMode === "new") {
    const opener = setInterval(() => {
      const button = [...document.querySelectorAll(".teams-head button")].find((b) => b.textContent.includes("New team"));
      if (!button) return;
      button.click();
      clearInterval(opener);
    }, 50);
  }
  // Open the demo chat and its first edit, as a person would have.
  const open = teamMode ? 0 : setInterval(() => {
    const row = [...document.querySelectorAll(".chat-row")].find((r) => r.textContent.includes("/health"));
    if (!row) return;
    row.click();
    clearInterval(open);
    const edit = setInterval(() => {
      const details = [...document.querySelectorAll("details.tool")].find((d) => d.textContent.includes("edit_fileapp.py"));
      if (!details) return;
      details.open = true;
      clearInterval(edit);
      setTimeout(() => document.querySelector("main.scroll")?.scrollTo(0, 1e6), 100);
    }, 50);
  }, 50);
})();
