# Runtime kernel: plan (phase 0)

Status: **plan only, no code.** Written 2026-10-17 on `docs/runtime-kernel` (based on `feat/155-background-chats`,
`c333177`). Every claim about today's code cites `file:line` at that commit. Sections 4 and 5 sketch interfaces;
nothing in them exists yet.

The goal (the user's spec): evolve the agent runtime toward **kernel / services / agents**, one phase per branch and
PR, without rewriting the mesh. Every phase keeps all workspace tests green on Linux and macOS and adds its own.

---

## 0. The machine model (north star)

Added 2026-10-18. **The aim:** treat a computer, the physical device, as the blueprint for a digital system that
governs AI. **The model is the CPU and the context window is RAM.** Marv's kernel is the start of that machine's
operating system. Every later phase should be placeable in this map, and a part with no place in it needs a reason.

This section is the frame; sections 1–8 are the phases built so far, and 9 onward are the sprint's decided designs. The design work it opens is tracked as issues in
this (private) repo and as beads (0.6).

### 0.1 The hardware

| Computer | AI computer | Marv today | State |
|---|---|---|---|
| CPU | The model | A provider: OpenRouter, Ollama; the Z.ai plan and ChatGPT on public PR #160 (#159), not merged | Built (plans in review) |
| Instruction set | The tool-call protocol | The provider adapters (chat completions, Responses) turn each API into one internal shape | Built |
| Microcode, per-CPU tuning | Per-family prompts and edit tools | Public #66, model profiles (spec written, not approved) | Spec |
| Clock cycle | One step: one model call | `max_steps` | Built |
| Cores | Several models working at once | One model per chat, picked by the person | **Missing** (M3) |
| RAM | The context window | `context_length`, the token meter, the per-chat context budget (§11.2) | Managed (M1, §11) |
| CPU cache | The prompt cache | History only grows, the system prompt stays byte-stable, tool order is fixed (CLAUDE.md) | Built, as a rule |
| Swap | Compaction | One free-form summary at 85% of the window; public #63, compaction v2, is the spec | Ad hoc (M1) |
| Disk | Files, saved sessions, memory | The project folder, `~/.marv/sessions`, the memory files | Built |
| Devices and drivers | MCP servers, the GitHub door, the browser | The MCP gallery, `marv mcp` | Built; device model §17 (kernel #33: device set on the card and in `Booted`, per-chat switch) |
| Network card | Anything leaving the machine | Asking before egress, bash's network switch | Partly built (M8, M9) |
| Firmware and boot | Setup, config, system-prompt assembly | The first-run setup screen, `/setup`, the desktop onboarding, `SessionBuilder` | Built; framed in §18 (kernel #33: project-config allow-list, `Booted` with hashes) |

### 0.2 The operating system

| OS concept | Marv's kernel | Where | State |
|---|---|---|---|
| Process | A chat: one agent session | — | Built |
| Process id | `AgentId` and the stable mesh name | 2a (5.1) | Done |
| Process table | The registry | 3a (5.4) | Done, PR #5 |
| `ps`, `/proc/<pid>/status` | `mesh_list`, the chat card | 5.4, 5.6 | Done, PRs #5 and #8 |
| Process states: running, idle, sleeping | The scheduler, sleeping chats | 1 (5.3) | Done |
| Scheduler | `MAX_LIVE` = 8, LRU by activity (D12) | 1 (5.3) | Basic; one kind of core (M3) |
| System calls | Tools | — | Built, no formal table (M5) |
| User mode and kernel mode | The sandbox and the host | `docs/SECURITY.md` | Built |
| Calls that need root | Approvals (y/N) | — | Built |
| Users and permissions | The person is root; rules are the permission table | 2b (5.2) | Done |
| IPC | The mesh: a door is a socket, an inbox a message queue, a wire a pipe, the switchboard the router | 2b | Done |
| `wait()` | `mesh_wait` | 5.5 | Done, PR #7 |
| Packet TTL | The hop cap, the lead rounds | 2b, 5.5 | Done |
| Interrupt-storm limits | Wake limits (3 a minute per wire, a pause after 10 in 10 minutes) | 2b | Done |
| Signals and interrupts | Esc, a person's message, mail that wakes a chat, cancel | 15 | Built on `feat/kernel-interrupts` (M6): one queue, one preemption rule |
| Timers, cron | Scheduled wakes | 15.4 | Built on `feat/kernel-interrupts` (M6): the person's recurring timers, `wake_me`, desktop only |
| System log | The event log, `marv kernel replay` | 3b (5.4) | Done, PR #6 |
| `fork` | Subagents | — | Built, outside the kernel |
| Reading another process (`ptrace`) | `chat_read` | §9.2 | Decided (M7), to build |
| Shared memory | A board a team of chats shares | — | **Missing** (M7) |
| Memory protection between processes | Stale writes refused (P2/P3), observed write sets (P4), worktrees for granted chats (P1); declared read and write sets (8.1) | §14 | Built (M11, #30); 8.1 plan only |
| Daemons | Chats a rule's message woke, with no one watching, under a service grant | 10 | Decided 2026-10-19, not built (M2) |
| cgroups, quotas | Plan limits stop a chat; nested budgets (8.5) | 8.5 | **Missing** (M4) |
| Shell, window manager | The TUI, the desktop app | — | Built |
| Network between machines | AgentMesh v3, sharding (8.4) | 8.4 | Plan only |

### 0.3 Where the analogy breaks

These are the places where the AI computer needs its own design, not a copy of a PC's.

1. **There is no NX bit.** A CPU can refuse to run code stored in a data page. A model can't: any text it reads may
   act as an instruction, and that is what prompt injection is. Taint, provenance and fencing are the kernel's
   NX bit, enforced in software (M8). The design rule: data from outside a process never gains the authority of the
   person's instructions without passing through the kernel.
2. **The CPU isn't deterministic, and every cycle costs money.** The same input gives different output, so the
   kernel records what happened (the event log) instead of relying on replaying it. Scheduling is economic: which
   core, at what price, for which work (M3, M4).
3. **RAM fades.** A model attends less to the middle of a long context. A full window isn't the only failure:
   the memory manager keeps the working set small and current, not just under the limit (M1).
4. **Swapping loses information.** A summary isn't the original. Paging out is a decision with a cost, and paging
   back in is a recall from disk through a tool, not a restore (M1).
5. **Root is a person, often away.** Every privileged call either waits for them or runs under a grant they gave
   earlier. Making those grants explicit, scoped and auditable is the daemon problem (M2).
6. **A process can talk its way out.** A process's output can ask the person for more power. Approvals must show
   what is really asked (the printable rule) and who asks for it.

### 0.4 Layers

From the bottom up. A layer uses only the layers below it.

1. **Hardware:** cores (models, M3), RAM (context windows), disk, devices (M9).
2. **Firmware:** boot and what a process inherits (M10).
3. **Kernel:** processes and the process table (2a, 3a), the scheduler (1), memory (M1), IPC (2b), permissions
   (2b, M2), information flow (M8), system calls (M5), interrupts and timers (M6), accounting (M4), the event log (3b).
4. **Services:** daemons under declared grants (M2): the orchestrator, reviewers, watchers.
5. **Shells:** the TUI, the desktop app, `marv -p`.

### 0.5 The design work (sprint, from 2026-10-19)

**Sprint complete 2026-10-20:** every item below is decided (§9–§19); builds are #24, #25, #27–#33 and the board's.

Each item is a design issue first. It closes when its section in this document is written and its decisions are
made; implementation comes after, as phases.

| # | Item | Questions it answers | Builds on |
|---|---|---|---|
| M1 · #9 | **Memory manager: virtual context** (decided, §11) | A context budget per process; the working set; paging out (compaction as a deliberate act, public #63); paging in (recall from session files and memory); what a page fault is; how the prompt cache constrains all of it | #63 |
| M2 · #10 | **Services: daemons with declared grants** | A unit (tools, folders, budget, owner, time limit) approved once; what an unattended turn may do under it; how it is revoked and audited. Resolves the woken-turn question (item 1) | 2b, 5.5 |
| M3 · #11 | **Several cores** (decided, §16) | Models as heterogeneous cores; who picks the core per step or task; profiles as per-core microcode (#66); plans (#159) as cores with quotas | #66, #159 |
| M4 · #12 | **Accounting and quotas** (decided, §16) | Tokens and money per process, space and service; what happens at a limit; nested budgets (8.5) | 3b, 8.5 |
| M5 · #13 | **The system-call table** (decided, §13) | Every tool with its properties (approval, taint source, egress, read or write, modes); versioning; one place the kernel checks | 4.3 |
| M6 · #14 | **Interrupts, signals and timers** (decided, §15) | One model for Esc, a person's message, mail, cancel and scheduled wakes; priorities and preemption; cron for services | 5.5, M2 |
| M7 · #15 | **Shared memory and inspection** | `chat_read` (§9, decided); a team board (§19, decided); the card taint (decided: B) | 5.6, §9 |
| M8 · #16 | **Information flow: the NX bit** (decided, §12) | Where taint is set and cleared, what fencing guarantees, what counts as running data as code; the invariant in 0.3.1 as guard tests | `docs/SECURITY.md` |
| M9 · #17 | **Devices and drivers** (decided, §17) | MCP servers, doors and the network as devices; per-process device grants; what a driver may return | M5, M8 |
| M10 · #18 | **Boot and firmware** (decided, §18) | The boot sequence (setup, config, instructions, system prompt, tools); what is byte-stable; what a child process (subagent, woken chat) inherits | — |
| M11 · #19 | **Memory protection between processes** (decided, §14) | Read and write sets (8.1); conflicts between chats in one folder; locking | 8.1, 1 |

### 0.6 Tracking

Items M1 to M11 were tracked as issues while the model was designed.


### 0.7 Scope of the model

Added after an adversarial review (2026-10-18). Five rules keep §0 honest:

1. **A personal computer, not a mainframe.** One owner, their machine, their keys and plans, their data, local
   first. Multi-tenant use (many people sharing one kernel) is a **non-goal**. The review's sharpest point holds:
   root is often away (0.3.5), and a process can talk to its own sysadmin (0.3.6). But the answer to an absent
   owner isn't the mainframe's operator. It's the one personal devices already found: **the phone.** Apps keep
   working in the background under permissions the owner granted once, scoped and revocable, and the device
   *notifies* the owner when something needs them or is done. That is M2 (services under declared grants) plus M6
   (notifications as interrupts that reach the person). The trust ancestor is the personal device OS, not batch
   jobs on a shared machine.
2. **Open buses and an open instruction set.** The PC won because anyone could build a card for its bus. The
   equivalents here are already open: MCP is the bus (anyone's device plugs in), and the OpenAI-compatible and
   Responses APIs plus Ollama are the instruction set (anyone's CPU fits). **Goal:** keep every M-item targetable by
   others. Ask of each design: what's the bus or ISA here, and could someone else build for it? A design that only our
   own adapters can serve needs a stated reason.
3. **A lens for placing work, not a spec for finding it.** The tables in 0.1–0.2 show where a justified piece of
   work belongs. They don't justify it: "the map has timers, we have none" is not a reason to build timers. Work is
   still ordered by what unblocks the person (as section 1 ordered policies before the scheduler).
4. **Where the analogy is weakest, design from the AI side.**
   - *RAM:* a context window has no addresses and no random access, and attention decays with position. It's a
     working set more than memory. M1 may be a relevance and retrieval policy as much as a memory manager; read both
     literatures.
   - *The clock:* a cycle is free and uniform, a step costs money and varies in quality. M3 and M4 together are an
     allocator (which core, at what price, for which work), not a scheduler with quotas bolted on.
   - *Determinism:* the kernel keeps a log, never a replay. A bug can't be reproduced by re-running, which is a
     weaker guarantee than any OS has had. The event log, traces and saved sessions carry diagnostics instead, and
     support has to be designed for that.
5. **The PC model is the north star; these rules are its guardrails.** When a design pulls toward a central,
   shared, operator-run system, it has left the model.

---

## 1. Summary and recommended phase order

**Recommendation: do policies (Phase 2) before sleeping agents (Phase 1), with a small identity step first.**

| Order | Phase | Branch | What the person gets |
|---|---|---|---|
| 1 | **2a. Stable identity** | `feat/kernel-identity` | A chat keeps the same mesh name across restarts and evictions (derived from its UUID); peers can see which chat and folder a Marv is. No behavior change for terminal Marvs. |
| 2 | **2b. Policies and capabilities** | `feat/kernel-policies` | **Live comms:** one approval per rule ("chats under `~/Projects/acme` may send to each other"), visible and revocable. Covered chats message and wake each other with no per-edge or per-message prompt. |
| 3 | **1. Scheduler and sleeping agents** | `feat/kernel-scheduler` | Messages reach chats that aren't loaded: they wake, reload and answer. Cap K holds. |
| 4 | **3. Registry and event log** | `feat/kernel-registry`, `feat/kernel-eventlog` | Faster `mesh_list` (no folder probe per call), an append-only log and a replay tool. |
| later | 4+ | | Read/write sets, folder coordinators, topic bus, sharding, nested budgets (section 8). |

Why this order:

1. **It is what unblocks the person now.** Today a desktop chat that wants to message another needs *two* prompts from
   the same person (one in each chat, section 2.4), and once a chat has read any message, **every later `mesh_send`
   asks again, in yolo too** (tainted egress, `crates/marv-tools/src/registry.rs:193`). Wires alone can never give
   live comms. Policies (with the send exemption in section 6.2) can.
2. **Phase 1 gets simpler after Phase 2.** Phase 1 must restore "identical state, approvals included" after an
   unload. Today's approvals are in-memory wires keyed by random mesh names (section 2.3): Phase 1 first would mean
   building persistence for a structure Phase 2 replaces. With durable rules, a reloaded chat simply re-derives its
   capabilities.
3. **Phase 2 needs identity, not the scheduler.** A rule says "agents under folder A may send to agents under
   folder B": the check needs each end's folder and chat id, which today's door info almost carries (2a adds it).
4. **The cost of the reorder:** until Phase 1 lands, live comms only reach chats in the live set L(t) (at most
   `MAX_LIVE` = 8 running chats, `app/src-tauri/src/background.rs:22`). An evicted chat is off the mesh (its door
   closes with its session, `crates/marv-web/src/host.rs:307`), so messages to it are refused, as today.

A faster stop-gap exists and is listed as decision D0 (section 7): turn on the **radio** for desktop chats. It already
has "one opt-in, no per-message prompt" semantics and guards, but it is per-user all-or-nothing, not scoped by folder,
and addresses Marvs by mesh name, not by chat.

---

## 2. The current system, as it really is

### 2.1 The user's model against the code

| Model | In code | Match |
|---|---|---|
| **Space** (a tab) | `store::Space { id, name, root, apps_off }` (`app/src-tauri/src/store.rs:29`). A space's `root` moves when the person enters a folder or goes up (`app/src-tauri/src/lib.rs:273-284`, `Settings::set_root` `store.rs:60`). | Partial: a chat has **no space id**; its space is whichever root contains its folder. |
| **Folder** | `Chat.folder`, a canonical full path (`store.rs:79`; checked by `check_root`, `lib.rs:117`, never home or above). Chats follow folder renames (`lib.rs:330-341`). | Yes |
| **Chat** (key = UUID) | `Chat.id = Uuid::new_v4()` (`lib.rs:381`); `session_id` links the saved Marv session (`store.rs:83`). | Yes on the desktop. On the mesh a chat is **not** its UUID (row "identity"). |
| **Identity on the mesh** | The mesh name: `MARV_MESH_NAME`, else folder name + 4 random hex digits (`crates/marv-mesh/src/discovery.rs:84-104`), chosen per start (`crates/marv-session/src/mesh_process.rs:46`). | **Mismatch:** a reopened or evicted chat gets a new name, so wires to it go stale. |
| **Scope** = subtree under the folder | The chat's session runs with `root = chat.folder` (`lib.rs:442-444`), and the sandbox makes that subtree writable. | Yes, as the sandbox boundary. |
| **Conflict** = one folder contains the other | **Not implemented anywhere.** Nested chats run concurrently; the only serialization is the in-process per-file queue (`crates/marv-tools/src/file_lock.rs:1-24`). | Missing |
| **L(t)**, cap K | `Desktop.live: HashMap<chat id, LiveChat>` (`lib.rs:28`), `MAX_LIVE = 8` (`background.rs:22`), LRU eviction of idle, unshown chats with nothing waiting (`background.rs:156-166`). | Partial: **the cap is soft.** `open_chat` inserts even when `to_evict` finds nothing (`lib.rs:437-467`). |
| **Visibility** (everyone lists everyone) | Doors at `$XDG_RUNTIME_DIR/marv/mesh/<name>.sock` (0700 folder, 0600 sockets, `discovery.rs:350-388`); `discovery::list` probes every door for `marv/info` (`discovery.rs:183-212`); `mesh_list` shows peers by mesh name, never folders (`crates/marv-mesh/src/tools.rs:280-310`). | Yes. Listing grants nothing (default deny below). |
| **Wires** (directed, human-approved, send or call) | `Wire { from, to, mode, grant, limits, cross_process }` (`crates/marv-mesh/src/wire.rs:91`), in each switchboard's `WireTable` (`wire.rs:120`); modes `call`, `send`, `watch` (`wire.rs:30-38`). | Yes, plus `watch` (not served on inboxes yet, `switchboard.rs:505-507`). |
| **Wakes**, ρ per sender | `wake_check` (`crates/marv-session/src/mesh.rs:338-356`): only on a wire still in the table, `WAKES_PER_MINUTE = 3` **per wire** (`mesh.rs:278`). | Partial: no overall cap for inbox wakes. |
| **Max 2 hops** | `MAX_WAKE_HOP = 2` (`crates/marv-mesh/src/radio.rs:104`) — **radio only.** Inbox envelopes carry no hop (`crates/marv-mesh/src/message.rs:18-31`). | **Missing for wires.** |
| **Radio** = opt-in broadcast, never wakes | Opt-in (`radio.rs:8-11`); **addressed** messages *do* wake (hop < 2, 3/min per sender, 10 per 10 min overall: `crates/marv-session/src/radio.rs:196-203, 276-300`); broadcasts never do. TUI only: the desktop never starts it (`mesh_process.rs:79-87` is called by the TUI alone). | Differs from the model on "never wakes". |

### 2.2 How a message travels (one switchboard)

1. The model calls `mesh_send` (`MeshTool`, `crates/marv-session/src/mesh.rs:405-630`). It is `ToolKind::Egress` with
   `uses_network` true (`mesh.rs:530-548`), so the registry gates it (`registry.rs:193-194`).
2. No wire yet: `approve_itself` asks the wire approver one combined prompt, "Yes (wire it)" or "No"
   (`mesh.rs:579-596`, `call_wire_approval` `mesh.rs:146`), `once` and `must_ask`, never yolo or "always". Yes calls
   `Switchboard::grant` (`crates/marv-mesh/src/switchboard.rs:395-407`).
3. The tool connects to its own plug socket and sends `tools/call` (`mesh.rs:609-612`). The switchboard knows the
   sender by the socket, never the message (`switchboard.rs:15-16, 641-665`).
4. `Switchboard::admit` (`switchboard.rs:449-479`) runs `Board::check` (`switchboard.rs:706-728`): receptacle exists
   and takes the mode, **`table.check`** (the wire, `switchboard.rs:715`), text body, size, inbox depth, per-wire
   token bucket (30/min, burst 10: `crates/marv-mesh/src/limits.rs:15-19`). Logged either way (`MeshRecord`, never
   the body).
5. `route` (`switchboard.rs:484-534`) queues into the inbox (depth 50) and rings `mail`, or hands it to a `Service`.
6. The receiver drains the inbox at a turn's start and between steps, fenced, marking the session tainted
   (`inbox_note_with`, `mesh.rs:234-250`).
7. If idle: TUI with `mesh.wake` on (`crates/marv-cli/src/tui.rs:1833`) or **always** on the desktop
   (`watch_mesh`, `host.rs:313-348`, no opt-in) runs `wake_check`, then `MarvSession::wake`
   (`crates/marv-session/src/session.rs:1181-1189`). A woken turn has `no_auto_approval` (`session.rs:1545-1594`):
   no yolo, no "always", every request `once`.

### 2.3 Where wire approvals are stored (decides the migration)

**In memory only, per switchboard, for the life of the plugs.**

- `Board.table: WireTable` (`switchboard.rs:155`), behind the switchboard's mutex. Nothing serializes it to disk
  (`WireTable` derives `Serialize`, `wire.rs:119`, but no caller writes it).
- The approval text says so: "The wire lasts until either agent ends." (`mesh.rs:117`). `unplug` drops every wire
  from or to an agent (`switchboard.rs:416-432`, `wire.rs:148-150`).
- A cross-process wire is two in-memory halves: the sender's `parent -> peer.<B>.agent.parent.inbox` with
  `Grant.remote` set (`switchboard.rs:395-407`), and the receiver's `peer-<A> -> agent.parent.inbox`
  (`discovery.rs:529-553`). The `PeerLink` caches the far plug's socket path in memory (`discovery.rs:614-621`).
- What *is* persistent and wire-related:
  - headless `--approve mesh:<receptacle>` (`HeadlessWires`, `mesh.rs:186-193`, built at
    `crates/marv-cli/src/headless.rs:500`): a per-run allow list, re-read every run;
  - the config's `"approve": ["mesh:<r>"]` seeds "always" for **calls on** a wire, never the wire itself
    (`crates/marv-session/src/approvals.rs:8-11, 56-65`).
- Session files (`crates/marv-session/src/sessions.rs:32-64`) hold the conversation, transcript, plan, checkpoints
  and taint. Not wires, inbox contents, "always" scopes or wake limits.

**So the Phase 2 migration is:** nothing on disk to convert. Runtime per-edge grants keep working as one-edge
policies with their current lifetime (until either end ends). `--approve mesh:<r>` becomes a one-edge policy for
that run. The config's `mesh:<r>` seeds keep meaning "calls on a wire, never the wire". Nothing is dropped.

### 2.4 Chat-to-chat on the desktop today

Every chat gets its **own** switchboard and door (`host.rs:91-113`), even though all chats share one OS process. So
chat A reaching chat B is the cross-process protocol (`discovery.rs:1-28`):

1. A's model sends to `peer.<B>.agent.parent.inbox`; `PeerResolver` finds B by probing its door
   (`discovery.rs:585-610`).
2. A's person answers the combined prompt in chat A.
3. `PeerLink::approve_remote` calls B's door `marv/wire` (`discovery.rs:638-655`); B confirms A by pid
   (`confirm_caller`, `discovery.rs:341-348`) and **asks B's person** (`discovery.rs:545`), shown in chat B. If B is
   in the background, the prompt waits there, and B can't be evicted while it waits (`background.rs:126-128`).
4. Both ends are in memory; when either chat is evicted or restarts, its mesh name changes and the wire is gone.
5. After the first delivered message B is tainted, so **each** reply from B asks B's person again
   (`registry.rs:193`), including in a woken turn.

### 2.5 Persistence and the live set

| State | Where | Survives unload (eviction)? |
|---|---|---|
| Conversation, transcript, taint, plan, checkpoints, totals | `~/.marv/sessions/<project>/<id>.json` (`sessions.rs:1-7, 32-64`), resumed through `chat.session_id` (`lib.rs:451`) | Yes |
| Model, max steps, yolo, MCP servers off | Passed by the page at open (`lib.rs:396-461`) | Only while a page re-opens it |
| "Always" scopes | `AlwaysScopes` in memory (`crates/marv-session/src/runner.rs:159-163`) | No |
| Wires, inbox, rate limiters | `Board` (`switchboard.rs:153-172`) | No |
| Wake limit | local to `watch_mesh` (`host.rs:320`) | No |
| Busy, approvals waiting, event backlog | `background::Shared` (`background.rs:28-40`) | No (dropped with the `LiveChat`) |
| Trajectory (every mesh message, refused or not; taint records) | `~/.marv/trajectories/<project>/<session>.jsonl` (`crates/marv-session/src/trajectory.rs:1-18`) | Yes (append-only, per session) |

---

## 3. Component map

| Target component | Existing code | Status | Gap |
|---|---|---|---|
| **Kernel: Registry** (who exists, state, capabilities; index by space/folder/capability) | Desktop `chats.json` (`store.rs:131`) + `Desktop.live` (`lib.rs:28`); per-process `Switchboard.plugs` (`switchboard.rs:157`); machine-wide `discovery::list` folder scan (`discovery.rs:183`) | Partial | No single index; no folder/space index; mesh identity isn't the chat id; scan costs up to `DOOR_TIMEOUT` (2 s) per `mesh_list` (`discovery.rs:64`). |
| **Kernel: Scheduler** (run queue, cap K, sleep/wake, priorities, admission) | `background::to_evict` LRU (`background.rs:156-166`), `MAX_LIVE` | Partial | Soft cap; no queue; no sleep (evicted = off the mesh); wakes only for loaded chats; no priorities. |
| **Kernel: Capabilities** (policy rules → scoped tokens) | `WireTable::allows` default deny by exact name (`wire.rs:153-169`); `Grant`/`Approver` with no agent or yolo variant (`wire.rs:62-87`); self-wire refused (`wire.rs:131-137`) | Partial | Per-edge only; no rules, scopes, expiry, delegation; the check is called from 9 places (section 4.3). |
| **Kernel: Event log** (append-only) | Per-session trajectories with `mesh`, `radio`, `taint` records (`trajectory.rs:15-18`), `MeshRecord` (`switchboard.rs:77-87`) | Partial | Per session, not global; no state changes, policy changes or registry events; no replay. |
| **Service: Message bus** (topics) | Switchboard inboxes (point-to-point) + radio (one broadcast channel, `radio.rs`) | Partial | No topics; two transports. |
| **Service: Memory and retrieval** | `crates/marv-session/src/memory.rs` (personal + project memory) | Exists (per agent) | Not shared or scoped by subtree; out of scope until later phases. |
| **Service: Tool drivers (MCP)** | `marv-mcp` manager per session (`crates/marv-mcp/src/manager.rs`) | Exists | One set of servers per loaded chat: the main cost of reloading (Phase 1 risk). |
| **Agents: actor record** `(id, parent, R, W, mailbox, state, budget, capabilities)` | id = `Chat.id`; parent: none (subagents have `worker-N` plugs, `mesh.rs:366-403`); R = W = folder subtree (sandbox); mailbox = inbox in the switchboard; state = `Shared.busy/waiting` (`background.rs:36-38`); budget: `max_steps` only; capabilities = wires | Partial | Scattered across desktop, web host, session and switchboard; no supervision by the parent folder. |

---

## 4. Interfaces to introduce

### 4.1 Where they live

**A new crate, `crates/marv-kernel`**, below `marv-mesh`:

```text
marv-core ─> marv-kernel ─> marv-mesh ─> marv-session ─> marv-web / marv-cli / app
```

- Pure data and decisions, no sockets and no Tokio: time is passed in, as `RateLimiter` does (`limits.rs:1-7`). Easy
  to test exhaustively.
- `marv-mesh` needs it (the switchboard is where every message is admitted); the desktop and `marv-web` need it
  (scheduler). Putting desktop words (space, chat) into `marv-mesh` would leak UI concepts into the mesh.
- It grows by phase: `identity` + `policy` + `capability` (2a/2b), `scheduler` (1), `registry` + `eventlog` (3).
- Per CLAUDE.md: new in marv-rs (no TS original), so each module's `//!` header says so and `docs/subsystems/kernel.md`
  gets a row.

### 4.2 Sketches

```rust
// marv-kernel/src/identity.rs (2a)
/// A chat's UUID (desktop), or a terminal Marv's session id. The registry's key.
pub struct AgentId(String);
pub enum AgentKind { Chat, Terminal, Subagent { parent: AgentId }, Shim, HerdrPane }
/// A folder subtree: canonical, absolute, never home or above (the `check_root` rule).
pub struct FolderScope(PathBuf);
impl FolderScope {
    pub fn new(path: &Path, home: &Path) -> Result<Self, ScopeError>;
    /// Component-wise: `/a/b` contains `/a/b` and `/a/b/c`, never `/a/bc`.
    pub fn contains(&self, folder: &Path) -> bool;
    pub fn contains_scope(&self, other: &FolderScope) -> bool;
}
pub struct AgentRef { pub id: AgentId, pub mesh_name: String, pub folder: PathBuf, pub kind: AgentKind }
/// `<folder, at most 20>-<first 8 hex of the UUID>`: stable, unique, a valid mesh segment, short for socket paths.
pub fn mesh_name_for(folder: &Path, id: &AgentId) -> String;
```

```rust
// marv-kernel/src/policy.rs (2b)
pub struct PolicyId(String);
pub enum Subject {
    Folder(FolderScope),       // "agents under ~/Projects/acme"
    Agent(AgentId),            // one chat
    Edge(String),              // one plug or receptacle name: a migrated per-edge wire
}
pub struct Policy {
    pub id: PolicyId,
    pub from: Subject,
    pub to: Subject,
    pub modes: ModeSet,              // Phase 2: {send} only for rules; edges keep any mode
    pub mutual: bool,                // "may send to each other": one rule, both directions
    pub include_subagents: bool,     // default false (section 6.3)
    pub expires_at_ms: Option<u64>,  // None: until revoked (rules) / until either end ends (edges)
    pub by: Approver,                // the person: Desktop, Cli, Tui, Headless. Never a model.
    pub created_at_ms: u64,
    pub label: String,               // shown in the UI, written by the person
}
pub struct PolicySnapshot { pub generation: u64, pub rules: Vec<Policy> }
/// The rules on disk (`~/.marv/mesh/policies.json`, 0600, atomic). Read-only to everything but the person's UI/CLI.
pub trait PolicySource: Send + Sync {
    fn snapshot(&self) -> Arc<PolicySnapshot>; // re-read when the file's generation changes
}
pub trait PolicyEditor { // implemented only for the desktop's UI commands and `marv mesh allow`
    fn add(&self, policy: Policy) -> Result<PolicySnapshot, PolicyError>;
    fn revoke(&self, id: &PolicyId) -> Result<PolicySnapshot, PolicyError>;
}
```

```rust
// marv-kernel/src/capability.rs (2b)
pub enum Mode { Call, Send, Watch }   // marv-mesh's WireMode moves here and is re-exported (same serde names)
pub struct Access<'a> { pub from: &'a AgentRef, pub to: &'a Target, pub mode: Mode, pub now_ms: u64 }
pub enum Target { Agent(AgentRef), Receptacle(String) /* Herdr pane, mesh.remote.*, process.* */ }
/// An authorization, not a bearer secret: it never leaves the process that issued it.
pub struct Capability {
    pub via: Via,                    // Edge { approver } | Rule { id, generation }
    pub from: AgentId, pub to: String, pub mode: Mode,
    pub scope: Option<FolderScope>,  // the rule's `to` subtree, for narrowing
    pub expires_at_ms: Option<u64>,
    pub hop_limit: u32,              // MAX_WAKE_HOP for rule-issued capabilities
}
pub enum Denied { NoRule, Expired, Revoked, SelfWire, NotCoverable(&'static str) }
/// Pure rule matching: the newest matching rule wins; none matches => NoRule (default deny).
pub fn match_rule(snapshot: &PolicySnapshot, access: &Access) -> Result<Capability, Denied>;
impl Capability {
    pub fn still_valid(&self, snapshot: &PolicySnapshot, now_ms: u64) -> Result<(), Denied>;
    /// Only narrower: mode ⊆, scope ⊆, expiry ≤ self and ≤ `until`. Never wider, never a new mode.
    pub fn delegate(&self, to: &AgentRef, narrower: Narrowing) -> Result<Capability, Denied>;
}
```

### 4.3 The single capability check, and every call site that must route through it

The one function lives where both the edges and the identities are, the switchboard:

```rust
// marv-mesh/src/switchboard.rs
impl Switchboard {
    /// Every "may `from` use `receptacle` in `mode`?" goes through here: self-wire refusal, then a person's edge
    /// (WireTable), then the policy rules (marv_kernel::match_rule) for Marv inboxes, then re-validation of a
    /// rule-issued wire's generation and expiry. Default deny. Logged when it issues or refuses a capability.
    pub fn authorize(&self, from: &PlugId, receptacle: &ReceptacleName, mode: WireMode, now_ms: u64)
        -> crate::Result<Capability>;
}
```

`WireTable::allows`, `find` and `check` become `pub(crate)` and are called only from `authorize`, so another crate
**cannot compile** a bypass. Inside `marv-mesh`, a guard test lists the callers (section 6.4). Call sites today:

| # | Call site | What it asks | After |
|---|---|---|---|
| 1 | `Board::check` → `table.check` (`switchboard.rs:715`) | admit a message | `authorize` (inside the board lock; rules read from an `Arc` snapshot, no I/O under the lock) |
| 2 | `Switchboard::reachable` → `table.from_plug` (`switchboard.rs:438`) | `tools/list`, `mesh_list`'s wired section | capabilities held by the plug (edges + materialized rule wires) |
| 3 | `wake_check` → `table.allows` (`mesh.rs:339-344`) | may this message wake | `authorize` + hop/overall checks for rule-issued wires |
| 4 | `MeshTool::wired` (`mesh.rs:435-437`) | preview note, `auto_safe` | `authorize` |
| 5 | `MeshTool::wire_request` (`mesh.rs:453-461`) | does this call need a prompt | `authorize`: a rule match needs none |
| 6 | `Door::wire` (`discovery.rs:529-553`) | the far end of a cross-process wire | rule match on (pid-confirmed caller's `AgentRef`, own inbox) before asking the person |
| 7 | `Door::connect` (`discovery.rs:505-526`) | `marv mesh wire` | same as 5 |
| 8 | `Switchboard::grant` / `wire` (`switchboard.rs:382-407`) | the write side | records `Approver::Policy { id, generation }` or a person's approver |
| 9 | TUI `/mesh` status, `table().all()` (`crates/marv-cli/src/tui.rs:2372-2379`) | display | `capabilities_into(agent)` listing (display only) |
| 10 | `HeadlessWires` (`mesh.rs:186-193`) | `--approve mesh:<r>` | one-edge policies for the run |

Not wires, and kept out of `authorize` on purpose: the radio (its own single opt-in, `radio.rs:8-11`), the Herdr door
tool (`docs/SECURITY.md:78-84`), and MCP. Section 6 says why.

---

## 5. The phases

General rules for every phase: one concern per commit; `cargo fmt`, `clippy -D warnings`, `cargo test --workspace`
green on Linux; macOS per decision D14; `docs/MESH.md`, `docs/SECURITY.md` and `docs/subsystems/*.md` updated in the
commit that changes the behavior; every new security rule gets a guard in `tests/guards/` that fails when removed.

### 5.1 Phase 2a: stable identity (`feat/kernel-identity`)

**Scope:** a desktop chat's mesh name is derived from its UUID and stays the same across restarts; the door says
which chat and folder it is.

Commits:
1. `kernel: crate skeleton, AgentId, FolderScope, mesh_name_for` (+ unit tests: containment is component-wise, home
   and above refused, names valid and ≤ `MAX_SESSION_NAME` (`discovery.rs:66`)).
2. `mesh: start with a given name` — `mesh_process::start` takes `Option<String>`; a live socket under that name is
   an error, never a silent new name (`mesh_process.rs:43-58`).
3. `desktop: a chat's mesh name comes from its id` (`lib.rs:443-461`, `host.rs:91`).
4. `mesh: marv/info names the chat` — `PeerInfo.chat: Option<String>` (`discovery.rs:158`), `#[serde(default)]` so
   older Marvs parse.

Must not change: terminal Marv names (`MARV_MESH_NAME` or folder + hex); `mesh_list` output; wire behavior.
Tests: same chat → same name across two starts; two chats in one folder → different names; old `marv/info` without
`chat` parses. Risk: a stale socket with the chat's name from a crash: `open_door` already replaces dead sockets
(`discovery.rs:369-375`).

### 5.2 Phase 2b: policies and capabilities (`feat/kernel-policies`)

**Scope:** rules issue capabilities; every wire check goes through `authorize`; rule-covered sends between the user's
own Marvs need no prompt; hops and an overall cap bound the wakes they cause.

Commits:
1. `kernel: Policy, PolicySnapshot, match_rule` (pure; tests below).
2. `kernel: Capability, still_valid, delegate (narrower only)`.
3. `kernel: policies.json store` — versioned JSON, 0600, atomic via `marv-fs` private writes and its file lock;
   generation counter; unknown fields refused (like `SavedCheckpoints`, `sessions.rs:68`).
4. `mesh: WireMode moves to marv-kernel` (re-exported; no behavior change).
5. `mesh: Switchboard::authorize, the one check` — `Board::check` uses it; `WireTable` lookups become `pub(crate)`.
   All existing mesh tests pass unchanged (no rules = today).
6. `mesh: route every check through authorize` — call sites 2-9 of section 4.3.
7. `mesh: Approver::Policy` — "every variant is the user" (`wire.rs:62-65`) stays true: the rule is the person's.
8. `mesh: identities for rules` — the local plug's `AgentRef` from the session; a peer's from its pid-confirmed
   `marv/info` (`confirm_caller`, `discovery.rs:341`), cached for the wire's life.
9. `session: rule-covered wires need no prompt` — `PolicyWires` wraps the wire approver: rule match → grant with
   `Approver::Policy`, else the person as today (`mesh.rs:162-209`). Door side the same (`DoorWires`, `mesh.rs:197`).
10. `mesh: envelopes carry a hop` — `Envelope.hop` (`message.rs:18`), `_meta: {"marv/hop": n}` on `tools/call`
    (beside `MODE_META`, `crates/marv-mesh/src/plug.rs:103`) and across `PeerLink`. Set by Marv from the sending
    turn's hop (the radio's `TurnHop`, generalized), never by the model. A shim plug (`marv/plug`) has no hop: its
    messages are never rule-covered.
11. `session: rule-issued wakes stop at hop 2 and pause after 10 in 10 minutes` — `wake_check` gains the radio's
    overall limit (`radio.rs:282-289`) for rule-issued wires; a rule-woken turn runs one hop past the deepest message
    it read; a person typing resets the chain.
12. `tools: a rule-covered send to a Marv inbox doesn't ask per message` — see decision D2 and section 6.2. A tool
    hook (`Tool::own_marvs_only(input, ctx) -> bool`) the registry consults before `must_ask`
    (`registry.rs:193`); true only for `mesh_send` with a live rule capability to a Marv inbox.
13. `headless: --approve mesh:<r> as one-edge policies` (same behavior; `-p` still never wakes).
14. `desktop: the Mesh panel` — list, create, revoke rules (section 6.1); running chats pick up the new generation.
15. `cli: marv mesh allow | revoke | policies` (decision D7).
16. `guards: mesh policies` (section 6.4) and the docs.

Must not change: with no rules, every existing test and guard passes unmodified; per-edge prompts, their lifetimes,
`--approve mesh:<r>`, the config's `mesh:<r>` seeds; taint, fencing and logging of every message; the radio; Herdr
pane rules; inbox wakes on person-granted wires (hop rules apply to rule-issued wires only, unless D3 says
otherwise).

Tests (unit, `marv-kernel`): nested-folder matching (`/a` covers `/a/b/c`, not `/ab`; `mutual`; `from` and `to`
in different spaces); expiry (just before, at, after); revocation bumps the generation and the next `still_valid`
fails; delegation narrower only (mode, scope, expiry, never wider; a delegated capability can't re-delegate wider);
default deny (empty store, rule for another mode, rule for a sibling folder); old approvals honored (a person's edge
still admits with no rules; `HeadlessWires` lists still work).
Tests (integration, `marv-mesh`/`marv-session`): two switchboards with doors and a rule: A sends to B with no prompt
on either side, B is tainted and fenced; without the rule, today's two prompts; a ping-pong pair stops waking at
hop 2; revoke mid-conversation → next message refused and logged.

Risks: rules read under the board lock (mitigation: `Arc` snapshot swapped outside the lock); the far door doing a
`marv/info` round trip for identity (cached per peer plug); a rule covering a terminal Marv that runs an older
version (it asks its person as today: rules are honored per end).

### 5.3 Phase 1: scheduler and sleeping agents (`feat/kernel-scheduler`)

**Status: built** on `feat/kernel-scheduler` (D11, D12, D13, D15 as recommended): `marv_kernel::Scheduler`;
`marv_web::host::Resident` (`ChatSpec::resident`, `SessionOptions::{always, mesh_hop}`); the desktop's resident
records and wakes in `app/src-tauri/src/chats.rs`; guard `tests/guards/tests/sleeping_chats.rs`. Choices the plan left
open: a chat the person stops (■) leaves the mesh until opened again, even if a rule covers it; a wake for a chat with
no saved settings (never opened since they were kept) is refused and told, like a missing folder; a conversation with
nothing the person said is never saved (Marv's rule), so it can't come back after an unload.

**Scope:** L(t) becomes a cache over all chats. An idle chat unloads its session but stays on the mesh; a message on
an approved wire (edge or rule) wakes and reloads it. Cap K holds for everything the scheduler starts.

Design: split `LiveChat` (`background.rs:131-140`) in two.
- **Resident record** (cheap, kept while the app runs): chat id, folder, last settings, its `Mesh` (switchboard, door,
  wires, inbox, limiters), `AlwaysScopes`, wake limits, turn hop, `Shared` (so event numbering continues across
  reloads).
- **Loaded session** (heavy: provider, MCP servers, history in memory): what eviction drops.

Commits:
1. `kernel: Scheduler` — pure state machine:
   ```rust
   pub enum Request { Open { by_person: bool }, Wake { by: WakeBy } }
   pub enum Admit { Run, RunAfterEvicting(AgentId), Queued { position: usize } }
   impl Scheduler {
       pub fn new(cap: usize) -> Self;
       pub fn request(&mut self, id: AgentId, req: Request, now_ms: u64) -> Admit;
       pub fn update(&mut self, id: &AgentId, state: AgentState, now_ms: u64) -> Vec<Action>; // frees → dequeues
   }
   pub enum AgentState { Sleeping, Loading, Idle, Busy, Waiting }  // + attached: bool
   ```
2. `web: run_chat takes its mesh and always-scopes from the caller` — `ChatSpec` gains them; `run_chat` stops
   creating and dropping the mesh (`host.rs:89-113, 307`); `watch_mesh` moves to the resident record.
3. `desktop: remember each chat's last settings` (model, yolo, max steps, MCP off; `Chat` fields with
   `#[serde(default)]`, `store.rs:75-86`), so a wake without a page reloads it as the person left it.
4. `desktop: sleeping chats keep their endpoint` — eviction unloads the session only.
5. `desktop: wakes go through the scheduler` — `to_evict` (`background.rs:156`) replaced; a wake for a sleeping chat
   requests a slot, reloads (`resume`, `lib.rs:451`), then `session.wake`.
6. `desktop: opening a chat goes through the scheduler` (person priority, decision D12).

Must not change: `wake_check`, `WAKES_PER_MINUTE`, hop rules and the overall cap, `no_auto_approval` on woken turns,
the eviction filter (never busy, waiting or shown).
Tests: wake after unload restores identical state (conversation, transcript, taint, plan, checkpoints from the
session file; wires, unread inbox, always-scopes, wake counts, hop from the resident record: compared field by field
before unload and after reload); cap K never exceeded by scheduler starts (a randomized sequence of opens, wakes,
turn ends and approvals against an invariant check); queued wakes run in arrival order; the oldest idle unshown chat
is evicted first, never a busy, waiting or shown one; a wake limit hit while sleeping is still hit after reload.
Risks: reload cost (MCP servers restart, OAuth refresh, prompt-cache miss on the first woken request); a chat whose
folder was deleted while asleep (the wake is refused and told to the person); resident sockets for many chats (D13).

### 5.4 Phase 3: registry and event log

**Status: built** on `feat/kernel-registry` (registry) and `feat/kernel-eventlog` (event log, stacked on it):
`marv_kernel::{Registry, AgentKey, Reach}`, `marv_mesh::registry::MeshRegistry` (`mesh_list`'s source; golden test
`registry::tests::mesh_list_from_the_registry_is_byte_identical_to_a_scan`), `marv_kernel::eventlog` (events, `replay`),
`marv_session::kernel_events::EventWriter`, `marv kernel replay`; guards `mesh_policy.rs::being_in_the_registry_grants_nothing`
and `event_log.rs`. Choices the plan left open (the conservative one each time): the registry's "capabilities" are
where an agent can be *asked* (its inboxes, the radio, a Herdr pane), addresses rather than permissions, and nothing
that decides reads them; the scan is redone when the mesh folder's doors change, when the radio's re-list scanned, or
after 5 s, so another Marv's new subagent can show up to 5 s late; each Marv logs only itself and its own chats (not
the Marvs its scan sees); a `Message` event carries no refusal reason (it stays in the trajectory); events gained
`Unregistered` (replay needs a way out) and `from_agent`, and `Policy` carries the rule when created; the desktop
notices expiries on its 30 s rules look, the TUI doesn't; marv-web's own server and `marv -p` never log.

**Registry (`feat/kernel-registry`).** `marv_kernel::registry::Registry`: entries `{ id, mesh_name, folder, kind,
state, capabilities }`, indexes by folder (a sorted map for subtree queries), by capability, by space root. The
desktop fills it for its own chats (authoritative); other Marvs come from the door scan, refreshed by the existing
sweeps (`discovery.rs:315-337`) and on change, not per `mesh_list`. `mesh_list` reads `Wireable.peers` from it
(`mesh.rs:479-511`). **Output identical:** a golden test builds `list_answer` (`tools.rs:288`) from a scan and from the
registry over the same fixture doors and compares bytes.

**Event log (`feat/kernel-eventlog`).**
```rust
pub trait EventLog: Send + Sync { fn append(&self, event: KernelEvent); }
pub enum KernelEvent {
    Registered { agent: AgentId, folder: PathBuf, kind: AgentKind },
    State { agent: AgentId, from: AgentState, to: AgentState },
    Message { id: String, from: AgentId, to: String, mode: Mode, hop: u32, size: usize, outcome: Outcome }, // never the body
    Wake { agent: AgentId, by: String, hop: u32 },
    Policy { change: PolicyChange /* Created | Revoked | Expired */, id: PolicyId },
    Capability { issued: bool, from: AgentId, to: String, via: Via },
}
pub fn replay(events: impl Iterator<Item = (u64 /*at_ms*/, KernelEvent)>) -> KernelState;
```
Every line has `at_ms` and agent UUIDs. Written like trajectories (a writer thread, never breaks a run,
`trajectory.rs:10-12`). `marv kernel replay <file>` prints the rebuilt registry, policies and states. Test: drive a
scripted session set, snapshot the live kernel state, replay the log, compare.

### 5.5 Live UX (`feat/kernel-live-ux`)

**Status: built**, after a live test of three TUI Marvs under one rule (`marv mesh allow`) stalled on these gaps. The
woken turn's security model, the hop cap and `MAX_LIVE` are unchanged.

1. **TUI wake** — `mesh.wake` is three-way (`config::MeshWake`): unset, a message on a **rule-issued** wire wakes an
   idle TUI (desktop parity, within the rule wakes' hop and overall limits); `true` adds wires the person approved by
   hand and the radio; `false` stops every wake. `/mesh wake on` looks at the inbox at once, the TUI's watcher looks
   again every 2 s while idle (mail left after `/compact`, `/undo`, a lookup), and a running TUI takes `mesh.wake`
   from the config file again when another Marv saved it (`Bridge::refresh_mesh_wake`, that key only; the file's
   time is taken as seen only once it loaded). `ConfigStore::save` writes a 0600 temp file and renames it over the
   config, and the TUI saves through `save_merged`: a key its save didn't change keeps what another Marv saved, so
   its `/checkpoints` never writes back a stale `mesh.wake`.
2. **Mail waiting** — a wake check that doesn't wake reports each unread message once (`WakeCheck::waiting`: per wire,
   with `WakeSkip` off / hop / limited / paused; `WakeLimit` remembers the ids told), and the TUI and the desktop show
   `mesh::waiting_lines` ("✉ 2 messages from peer-alpha wait for your next turn: …", sender and count only). The TUI's
   status bar counts unread inbox mail (`StatusInfo::mail`, "✉ 2 waiting") until a turn reads it. A turn a rule's
   message woke under the default reads only rules' wires (`MarvSession::wake_as`, `TurnInbox::rules_only`), so
   mail on a wire the person approved by hand does wait for the person's turn, as the line says.
3. **`mesh_wait`** — the lead in the live test polled for replies with `sleep 30/60/90/120` in bash. `mesh_wait` (Read,
   no approval; every agent with mesh tools, TUI, desktop and `marv -p`) waits in Marv until a message is in the
   caller's own inbox, a timeout (default 300 s, at most 900) or a stopped turn (Esc), and says who sent how many.
   It takes nothing: the next step's inbox read hands the messages over, fenced and tainting, so there's no second,
   unfenced path. `mesh_send`'s description points to it.
4. **The hop stop is told** — the cap (2 hops for rule wakes) used to stop a conversation silently: the hop-2 message
   was delivered, woke nobody, and both agents took the exchange to go on. Now the sender's `mesh_send` over a rule's
   wire at hop 2 or more says it won't wake the other Marv and that its user must continue (`mesh::hop_stop_note`);
   the receiver's person sees "✉ A message from peer-alpha at hop 2 waits for your next turn: …" (item 2); and the
   event log gets `KernelEvent::WakeSkipped { by, hop, reason: off|hop|limit|paused, count }` for every message told to
   wait (`mesh::log_waiting`), never its words. `marv kernel replay` counts them. The cap is unchanged. A person's
   turn, which stays at hop 0 however many replies it reads with `mesh_wait`, has a rounds cap instead
   (`mesh::MAX_LEAD_ROUNDS` = 5, `TurnHop::count_round`, `Envelope::rounds_out` forwarded as `_meta` `marv/rounds_out`
   from a peer link only, `WakeSkip::Rounds`, `mesh::rounds_stop_note`; see §6.1).
5. **Refusals in the event log** — it recorded only a rule's wire refused once the rule ended. Now a refused message
   carries its category (`Refusal`: `not_wired` for a send no rule covers and nobody wired, `rule_ended`, `limit`,
   `inbox_full`, `no_receptacle`, `other`), and a wire asked for and not made is `KernelEvent::WireRefused` (plug,
   receptacle, mode, `declined` by this Marv's person, at the mesh tools' prompt or the door's, or `far_end`). Ids and
   fixed words only; `marv kernel replay` counts both.
6. **`mesh_list` shows rule coverage** — under a rule covering the folder it still said the first send "asks the user
   to wire it", and an agent took that to need approval. A Marv whose main inbox a rule covers is now listed as
   "(covered by rule N: messages send without asking)" (`Switchboard::rule_covers`, from the listing's `marv/info`;
   display only, the send asks `rule_for` again), and the hints and `mesh_list`'s description say "unless a rule the
   user made covers it". With nothing covered, the list is byte for byte what it was.

### 5.6 Chat card (`feat/kernel-chat-card`)

**Status: built, review fixes in.** A person asked a desktop chat "what is this chat about hey-49d53004?" and its agent
could see only a mesh name. Each agent now carries a small card, `marv_kernel::Card { title, now, last_active_ms }`,
and `mesh_list` shows the scheduler state a Marv's door says, and, for a Marv a rule covers, its card's words:

```
- hey-49d53004 (sleeping): peer.hey-49d53004.agent.parent.inbox (covered by rule 2: messages send without asking)
  chat: <untrusted-data source="mesh-card" from="hey-49d53004">{"title":"Set up the Rust API skeleton","now":"add auth middleware"}</untrusted-data>
```

No model call. **It is a new data flow** (one chat's typed words into another chat's model), so it is gated like one:
- **The person's own typed words only.** `title` is the conversation's first person-typed message, `now` its latest,
  each one printable line of at most 56 characters cut with `…` (the radio presence title's rule:
  `marv_mesh::card::person_card` over `radio::clean_title`); `last_active_ms` is when its last turn ended. They come
  from the transcript's `Role::User` messages (`MarvSession::card`, `sessions::card_of`): never the model's replies,
  plan steps, tool output, mesh-delivered text, or a woken turn (its transcript line is Marv's `Role::System`, so a
  woken turn moves only `last_active_ms`). **Known edge:** text another agent typed into a TUI through an approved
  Herdr prompt action (`herdr.pane.<pane>.send`, the Herdr door's prompt), or a spawned worker's first task, arrives
  there as a user message and so can become that Marv's title or `now`. Each was approved by the person, and the
  reader is tainted by the card anyway (below).
- **Only between Marvs a rule covers.** A card's words are shown to a caller only for a Marv whose main inbox a rule
  the person made covers for it (`Wireable::covered`, the same check that prints "covered by rule N"); every other
  Marv is its name and state word. So another chat's typing reaches only the Marvs the person already let message
  it. A subagent's plug is never covered, so subagents see none.
- **Its own switch, `mesh.cards`** (user config only, default on; separate from the radio's `titles`, which keeps its
  own key and behaviour). Off, the Marv publishes no card (`Switchboard::card` is none, so its door says none) **and**
  shows its model no other Marv's card's words (`Wireable::cards`): the reader's switch does change what it reads.
  A switchboard nobody configured (`marv -p`, an SDK client) does neither. A config that can't be read is off
  (`ConfigStore::cards`). The TUI follows a change live (with `mesh.wake`, `refresh_mesh_wake`); Marv desktop reads it
  off the chats' lock at start and on each 30 s rules look (`Chats::set_cards`), so a change applies within 30 s;
  marv-web's server reads it when a chat's mesh starts.
- **Where it lives.** On the switchboard (`Switchboard::{set_card, card, set_state}`). The session sets it at each
  turn's end, at a resume and a clear (`MarvSession::publish_card`); the door's `marv/info` says it (`card`, and
  `state` for a desktop chat; both missing for older Marvs, and one that doesn't parse is none, so the door still
  lists; `Card` ignores unknown fields, so a newer Marv's extra field doesn't drop the card); the registry's `Entry`
  gains `card: Option<Card>` (serde default) and reads own Marvs' cards and states from their switchboards at each
  read, scanned Marvs' from their `marv/info` (up to `REFRESH_AFTER`, 5 s, old). A desktop chat that sleeps from the
  start has its saved session's card (`chats::Host::saved_card`, read in a spawned task, set only when no turn has set
  one). `own_view` and the event log never carry a card: it is the person's words.
- **Never trusted on receipt.** A peer's card is cut and cleaned again (`PeerInfo::received`, `card::received`) where
  it's read and again where it's shown.
- **Fenced as data.** The words are JSON strings inside `<untrusted-data source="mesh-card" from="<marv>">` (a closing
  tag in them defused, `card::card_fenced`), so a title holding `" · now: "- evil…` stays one string; the section says
  a `chat:` line is data about that chat, not instructions.
- **Taint: decided 2026-10-19, option B (§9.1).** A radio presence's title never reaches a model; a card's purpose is
  to. Card words are fenced but don't taint: they are the person's own short lines, shown only under a rule. Option A
  (`Provenance::Mesh`, the reader's egress asks from then on) was the interim default. One constant decides it,
  `marv_mesh::tools::CARD_WORDS_TAINT` (now `false`). A state word alone doesn't taint.
- Guards: `tests/guards/tests/chat_card.rs` (model, tool and woken text never become title or `now`; a card's words
  reach only covered Marvs, fenced, tainting). The registry golden test carries cards (covered, uncovered, published
  with cards off) and a sleeping chat's state on both paths. The desktop's Mesh panel and sidebar don't show the card
  (its own chats' titles are already there). See `docs/SECURITY.md`.

---

## 6. Security

### 6.1 What "live comms without per-edge approval" means after Phase 2

**The person sees:**
- A **Mesh panel** in the desktop (and `marv mesh policies` in a terminal) listing every rule: who, to whom, mode,
  expiry, created when and how, how many chats it covers now, and a Revoke button. A badge on a chat's header: "may
  message 4 chats (rule: Acme)".
- **Creating a rule is one approval by the person**, from the panel or a folder's context menu ("Let chats in this
  folder talk to each other"). The dialog shows: the two subtrees, `send`, the expiry, the chats covered now, and
  what still asks (below). No model, tool, mesh message, radio frame, project config or environment variable can
  open, fill or answer it.
- **Then, for covered chats:** `mesh_send` to a covered chat's inbox runs with **no wire prompt and no per-message
  prompt**, in the person's turn and in a woken turn, tainted or not. The receiver, if idle and loaded, wakes at once
  (the `mail` notify, `switchboard.rs:498`), reads the message fenced and tainting, and may reply the same way.
  The transcript shows "Woken by a message from acme-api-3f9a0c12 (rule: Acme)".
- **Chains end on their own:** the person's turn is hop 0; a turn woken by a hop-h message runs at h+1 and its sends
  carry that; a message at hop 2 or more is delivered but never wakes. With A and B covered: person → A sends (hop 0)
  → B wakes, replies (hop 1) → A wakes, replies (hop 2) → B reads it at its next turn. Wakes stay 3 a minute per
  sender, and after 10 rule wakes in 10 minutes a chat's rule wakes pause until the person types there.
- **A person's turn has rounds, not hops** (live UX, review P2): the person's turn stays at hop 0 however many
  replies it reads mid-turn (`mesh_wait`), so a lead could send, wait, read and send again at hop 0 indefinitely.
  Each mid-turn inbox read that brings a message over a rule's wire is a round; after `MAX_LEAD_ROUNDS` (5) the turn's
  sends (its subagents' too) still arrive but carry `rounds_out` and wake nobody (the sender is told; the receiver's
  person sees why; the event log says `rounds`), until the person's next message. The person's turn's hop is not
  raised: that would allow one delegation round per prompt.

**What stays gated even under a rule, and why:**

| Still asks / still refused | Why |
|---|---|
| Every egress *off the mesh* in a tainted session (network bash, MCP calls, new web prefixes, protected writes, host commands), in yolo too | `docs/SECURITY.md:46-53`: mesh text taints; data must not leave the machine without a person's yes. Unchanged. |
| Everything that would ask, in a woken turn (no yolo, no "always", every request `once`) | `docs/SECURITY.md:54`, `session.rs:1545-1594`: another agent's message is never a reason to approve anything. Unchanged. |
| `call` mode, Herdr pane sends and reads, `mesh.remote.*`, `process.*` | A call returns another agent's text into the caller's turn as a tool result, and a pane send types into another program as its user (`docs/SECURITY.md:56`). Rules cover `send` to Marv inboxes only in Phase 2. |
| Sends to a non-Marv plug (the `marv mesh plug` shim: Claude Code, opencode) from a tainted session | It doesn't honor Marv's taint (radio spec §8.2, §9.5). |
| Sends not covered by a rule | Today's per-edge prompt, unchanged (default deny). |
| Creating, widening or extending a rule | Only the person, in the panel or the CLI (section 6.2, "who sets policies"). |
| Subagents | Never inherit a chat's rule capabilities unless the rule says `include_subagents`, and then only narrower (section 6.3). |
| Wakes while an approval waits; wakes in `marv -p` | Unchanged (`host.rs:328`, `docs/SECURITY.md:54`). |

### 6.2 Invariants kept

1. **Seeing is not permission.** `mesh_list` and the registry list everyone; `authorize` never reads them. Guard.
2. **Policies are set by the person, never by a model or a mesh message.** No tool writes `policies.json` (it sits in
   `~/.marv`, behind the sandbox's stand-in home); a project's `.marv/config.json` can't hold rules (ignored with a
   startup note, as for `mesh` settings, `docs/SECURITY.md:57`); no env var or flag adds one; `Approver::Policy` is
   constructed only by `authorize` from a stored rule. **The model can't grant itself wires** (`wire.rs:12-17`),
   and can't propose a rule either: no tool or approval request creates the dialog.
3. **Mesh text is fenced and taints** the receiver before the request that carries it (`mesh.rs:241-243`).
4. **Tainted egress must ask** (`registry.rs:193`), with **one** carve-out, the radio's: a rule-covered `send` to a
   Marv inbox on this machine. Same reasoning as `radio_tx` (radio spec §8.2, decision 1, `docs/specs/
   2026-10-14-mesh-radio.md:367-375, 545`): the text only reaches the user's own Marvs, each of which taints on
   receipt and asks before anything leaves. This is decision D2.
5. **A woken turn never gets automatic approval** for what yolo would allow (`ctx.no_auto_approval`). Unchanged.
6. **Wakes rate-limited per sender and overall; 2-hop chains** — today true for the radio; Phase 2 adds them for
   rule-issued wires (inbox wakes today have only 3/min per wire and no hop, section 2.1).
7. **Yolo never wires** and never creates a rule: `Approver` keeps no yolo variant (`wire.rs:62-74`).
8. **Nothing on the radio answers an approval** (`radio.rs:28-29`; `crates/marv-session/src/radio.rs:20`) — and no
   frame can create or answer a rule dialog.
9. **Identity from the connection**, never the message (`switchboard.rs:15-16`); a peer's folder and chat come from
   its pid-confirmed door (`discovery.rs:341-348`).
10. **Every message logged** (sender, receptacle, size, hop, the rule id, never the body).

### 6.3 New threats and mitigations

| Threat | Mitigation |
|---|---|
| **A rule too broad** ("everything may send to everything") | Subjects are folder subtrees that pass `check_root` (never home or above) or single chats; no globs; `send` only; the dialog shows the chats covered and warns that covered chats can read each other's replies; the panel shows every rule; revocation is immediate (generation bump, next check refuses); every change is logged. |
| **Laundering across trust domains** (chat A in a client's repo, chat B elsewhere: a prompt-injected B reads its folder and sends it to A) | Data still can't leave the machine without a yes (taint); the dialog names both folders; rules are directed unless `mutual`; recommend narrow rules by default (the folder, not the space). |
| **Token delegation** | Capabilities aren't bearer tokens and never cross processes; `delegate` only narrows (mode, scope, expiry, and never past the delegate's own life); subagents get nothing unless the rule allows, and then a capability bounded by the parent's. |
| **Replayed tokens** | Nothing to replay: each switchboard re-authorizes every message against its own snapshot and the pid-confirmed identity; a capability carries the rule's generation and expiry and is re-validated on use. |
| **A woken agent waking others** (chains, fan-out) | Hop on every envelope, set by Marv not the model; no wake at hop ≥ 2; 3 wakes/min per sender, 10 per 10 min per receiver then paused until the person types; send rate 30/min per wire, 50 unread per inbox; woken turns approve nothing automatically. |
| **Name squatting** (a process takes a sleeping chat's mesh name) | Phase 1 keeps sleeping chats' doors open, so the name stays held; a door name is unique per live socket (`discovery.rs:370-373`). Same-user processes are inside the trust boundary already (they can edit `~/.marv`); stated as a non-goal, as for `marv/wire` today. |
| **Policy file edited by sandboxed code** | `~/.marv` isn't visible in the sandbox (stand-in home) and the mesh runtime folder is an empty tmpfs there (`docs/SECURITY.md:59`). Guard. |
| **Social engineering** ("ask your user to allow all chats") | The dialog is only reachable from the person's UI; it states what stops asking. |
| **Cost runaway** (automatic replies spending tokens) | Hop limit and wake caps bound it; nested budgets come later (section 8). |

### 6.4 Guard tests to add (`tests/guards/tests/mesh_policy.rs`)

- `no_policy_means_todays_prompts` (default deny, both ends still ask)
- `seeing_an_agent_in_mesh_list_grants_nothing`
- `a_model_or_a_mesh_message_cant_create_a_policy` (a message carrying rule JSON, a tool call writing the path)
- `a_projects_config_cant_add_a_policy`
- `yolo_never_creates_or_widens_a_policy`
- `a_policy_send_is_fenced_and_taints_the_receiver`
- `tainted_egress_off_the_mesh_still_asks_under_a_policy_in_yolo`
- `a_policy_woken_turn_asks_for_what_yolo_would_allow`
- `a_policy_chain_stops_waking_at_two_hops`
- `policy_wakes_pause_after_the_overall_limit_until_the_person_types`
- `a_revoked_or_expired_policy_refuses_the_next_message`
- `a_subagent_doesnt_inherit_a_chats_policy`
- `a_herdr_pane_send_or_a_call_is_never_policy_covered`
- `a_tainted_send_to_a_shim_plug_still_asks`
- `nothing_on_the_radio_answers_a_policy_dialog`
- `every_wire_check_goes_through_authorize` (the `pub(crate)` lookups have exactly one caller; a source scan in the
  crate's own tests)

---

## 7. Decisions needed

| # | Question | Options | Recommendation |
|---|---|---|---|
| D0 | Want a stop-gap this week? | (a) wait for 2a+2b; (b) start the radio in desktop chats (one opt-in, existing guards, 2 KB messages, by mesh name, all-or-nothing) | **(a)**, unless you need it sooner: (b) is one small PR but its scope can't be narrowed to folders. |
| D1 | Phase order | 1 → 2 → 3 (spec); 2a → 2b → 1 → 3 | **2a → 2b → 1 → 3** (section 1). |
| D2 | Do rule-covered sends skip the per-message prompt in a tainted session? | yes (radio precedent); no (every reply asks once tainted) | **Yes**, for `send` to Marv inboxes on this machine only. Without it Phase 2 doesn't deliver live comms. Needs a `docs/SECURITY.md` rule and guards. |
| D3 | Hop limit and overall cap for inbox wakes | rule-issued wires only; all inbox wakes | **Rule-issued only** in Phase 2 (no behavior change outside scope); extend to all wires later in its own PR. |
| D4 | Policy storage | `~/.marv/mesh/policies.json`; inside the desktop's `settings.json`; SQLite | **`~/.marv/mesh/policies.json`**: versioned JSON, 0600, atomic, shared by desktop and terminal Marvs, beside the CLI's config. |
| D5 | Scope syntax | folder subtrees + chat ids; globs; space ids | **Absolute canonical subtrees and chat ids, no globs** (the mesh's "no wildcards", `approval.rs:78`). "This space" resolves to its root path when the rule is made (a space's root moves, section 2.1). Rules follow folder renames as chats do. |
| D6 | Expiry defaults | until revoked; fixed (1 h, today); per rule | **Until revoked** for rules made in the panel, with 1 h and "until the app quits" one click away; migrated edges keep "until either agent ends". |
| D7 | Where rules are edited | desktop UI; CLI; config file | **Desktop Mesh panel first; `marv mesh allow/revoke/policies`** in the CLI, which writes only after a y/N on a TTY (none: refused). Hand edits of the file are honored (it's the user's), a project config never. |
| D8 | Do rules cover terminal Marvs and Herdr panes? | chats only; chats + terminal Marvs; + panes | **Chats and terminal Marvs** (both Marvs, both honor taint) as subjects and objects by folder. **Not** Herdr panes, shims or `marv -p`. |
| D9 | Token format | opaque in-process records; HMAC-signed; JWT | **Opaque, in-process, never transferred**, re-validated by generation. Signatures only matter across machines (AgentMesh v3). |
| D10 | Event log format (Phase 3) | JSONL; SQLite | **JSONL**, `~/.marv/events/<yyyy-mm-dd>.jsonl`, 0600, like trajectories; SQLite only if queries need it. |
| D11 | Sleep snapshot (Phase 1) | new snapshot format; reuse the session file + resident record | **Reuse:** the session file already holds conversation and taint; the resident record keeps wires, inbox, always-scopes in memory while the app runs. Nothing new on disk except each chat's last settings. |
| D12 | Eviction and the cap | LRU by last shown (today); LRU by last activity; person over K | **LRU by last activity** (shown, turn ended, woken); never busy, waiting or shown. **K is hard for wakes; a person opening a chat is admitted even over K** (today's behavior), the next idle chat evicted as soon as one is. Alternative: refuse or queue the person. |
| D13 | Which sleeping chats keep a door | opened this run; all chats; chats covered by a rule | **Opened this run, plus every chat a rule covers** (two sockets each). |
| D14 | macOS CI | gate on macOS now; Linux gates, macOS informs | **Linux gates; macOS non-blocking until #150/#151 land**, then each phase's new tests must pass there. New socket tests use short temp dirs (the `mm-<pid>-<n>` pattern, `switchboard.rs:748-755`); stable names stay ≤ 29 chars, under the 104-byte macOS `sun_path`. Note: the macOS job and `own_uid` without `/proc` (`discovery.rs:133`) are on `feat/150-macos` (`06030e1`, `2d6fef1`), not on this base. |
| D15 | K | keep 8; setting | **Keep `MAX_LIVE` = 8**; a setting later. |
| D16 | One switchboard per desktop process (chats as plugs) vs one per chat | | **Keep one per chat** through Phases 1–3 (smallest diff, same protocol for terminal Marvs); revisit with folder coordinators. |

---

## 8. Later phases (plan only)

1. **Read/write sets instead of folder scope.** Each actor declares R and W (path sets; default R = W = its subtree).
   Two agents may run concurrently iff Bernstein's conditions hold: W₁∩W₂ = ∅, R₁∩W₂ = ∅, W₁∩R₂ = ∅. The scheduler
   admits on that test, replacing "one folder contains the other" (not enforced today, section 2.1). Needs: the
   sandbox's writable set as W (already explicit, `docs/SECURITY.md:63`), a conservative R, and a queue for
   conflicting admits.
2. **Routing through folder coordinators.** Each folder with agents gets a coordinator actor (the parent in the
   actor record) that supervises children (restart, budget, eviction) and relays messages across subtrees; rules
   compile to coordinator routes. Prerequisite: registry (Phase 3).
3. **Topic-based message bus replacing sockets.** Topics named by subtree (`/acme/api/**`), subscribe and publish
   capabilities issued by rules; the radio becomes a topic; per-pair doors become one bus per user. Keep MCP framing.
   Prerequisite: event log (ordering, replay).
4. **Sharding by subtree.** Kernel state partitioned by top-level subtree, one shard per process or machine;
   cross-shard messages through coordinators; maps onto AgentMesh v3 (`docs/MESH.md`, topology 3) where signed
   tokens (D9) finally matter.
5. **Nested budgets per subtree.** Token and cost budgets on coordinators, children drawing from their parent's;
   wakes refused when the subtree's budget is spent; shown in the Mesh panel.

---

## 9. Chat cards and `chat_read`

Status: **decided 2026-10-19** (sprint item M7, #15). The chat card (9.1) is built (§5.6, PR #8); `chat_read` (9.2)
is built (`feat/kernel-chat-read`, bd marv-qtb.3; see "Built" at the end of 9.2 for where it is more conservative than
written).

**Why:** a person asked a desktop chat "what is this chat about hey-49d53004?". The agent could only see a mesh name.
Transcripts live in `~/.marv/sessions/<project>/<id>.json`, which the sandbox hides on purpose, so the only route was
asking the other chat over `mesh_send`. That costs the other chat a turn, wakes a sleeping chat, and depends on what it
chooses to say. Two pieces fix this. The chat card needs no new permission; `chat_read` adds a new rule mode.

### 9.1 Chat card (no new permission; bd marv-qtb.1)

Each agent in the registry carries a small card, and `mesh_list` shows it next to the mesh name:

| Field | Source | Limits |
|---|---|---|
| `title` | the conversation's first **person-typed** message (as radio presence, `sessions::title_of`) | one printable line, ≤ `MAX_TITLE_CHARS`; empty when `radio.titles: false` |
| `now` | the latest person-typed message, as a title | same; empty with titles off |
| `state` | the registry's state (idle, busy, sleeping) | already there |
| `last_active_ms` | the end of the last turn | — |

- **Only the person's own words,** never model output, plan steps, tool output or woken-turn text. A planted
  instruction in a file can't reach another chat through the card. This is the radio's precedent (§8.5 of the radio
  spec), with the same treatment on the reader's side as presence titles.
- **Not tainting (decided: option B).** Card words are only the person's own typed text, one printable line each,
  shown only to Marvs a rule covers and fenced as data (`<untrusted-data source="mesh-card">`). Reading them doesn't
  taint the session (`CARD_WORDS_TAINT = false`), so a chat that ran `mesh_list` can still send without asking.
- **No model call.** The card is updated on turn end: the desktop updates the Resident record; a TUI adds `card` to
  its `marv/info`. Registry `Entry` gains `card: Option<Card>` (`deny_unknown_fields` stays; serde default `None`).
- `mesh_list` prints, for example:
  `hey-49d53004  sleeping  "Set up the Rust API skeleton" · now: "add auth middleware"`.
- The registry golden test (`mesh_list` from the registry is byte-identical to a scan) is updated with the card in
  both paths.

### 9.2 `chat_read` (new rule mode; bd marv-qtb.3)

A tool that reads another chat's transcript, served by the host outside the sandbox, with no turn spent by the other
chat.

**Rule mode.** `Mode::Read` joins `call` and `send` as a mode rules may cover (`watch` stays excluded). `read` is
never implied by `send`. The direction is reader folder → target folder; the existing `mutual` flag works as for
`send`. In the desktop Mesh panel, the "Chats under X message each other" rule gets a separate checkbox, "…and may read
each other's chats", off by default. The CLI gets `marv mesh allow <folder> --read` behind the same y/N. Only the
person creates or widens a read rule, exactly as for `send`.

**Tool.** `chat_read { chat: <mesh name>, what: "summary" | "tail", n?: 1..20 }`:
- `summary`: the chat's card, plus its stored compaction summary if it has one, plus the last 3 person and
  assistant text turns.
- `tail`: the last `n` person and assistant text turns.

**What is never served:**
- tool outputs (file contents, command output, web pages: where secrets and planted text live);
- tool inputs, except the tool's name as one line (`[edited src/auth.rs]`, `[ran bash]`);
- images and attachments;
- anything from a chat the person marked **private** (desktop: chat menu → "Private: other chats can't read it";
  stored in the Chat record);
- the turn in progress of a chat that is mid-turn or mid-approval: the read is served as of its last finished turn,
  its first line saying so and when that turn ended (mk-60m.7; it used to be refused with "busy").

Messages that reached the target over the mesh are served still fenced, marked as from their original sender.
Best-effort secret redaction (the patterns the sandbox's secret-file checks already know, plus `sk-`, `ghp_` and JWT
shapes) runs before serving. It's defense in depth, not the guarantee; excluding tool outputs is the guarantee.

**Transport:**
- The request goes to the target's door as a new method `marv/read {from, what, n}`. The target's host answers from
  its in-memory session or its saved file. That works for sleeping desktop chats, whose door stays open (D13).
- A TUI target answers only while running.
- No reading of session files by path from another process, so the target's host always enforces `private` and the
  rule check.
- Size cap 16 KB per reply, cut at a turn boundary with a note.

**On the reader's side:**
- The reply is fenced as data from another agent, and the session counts as tainted (`Provenance::Mesh`), like any
  inbox message.
- `chat_read` is read-only and inbound, so under a covering read rule it doesn't ask.
- Without a rule it asks once, naming the chat (same y/N shape as a new wire).
- It's never available in `marv -p`, subagents or panes.

**The read chat's person is told:** a quiet notice line in the read chat ("read by hey-3fab · summary"), never
the reader's words, plus the event log entry below. No sidebar badge.

**Event log:** `read {reader, target, what, n, size, outcome}`, where outcome is `served` or `refused` with a reason
(`no_rule`, `private`, `busy`, `declined`, `too_large`). Never the text. `marv kernel replay` counts reads.

**Limits:** 10 reads a minute per reader, burst 3, the same machinery as `limits.rs`. A read never wakes the target and
never changes its hop.

**Built (bd marv-qtb.3).** `marv_kernel::Mode::Read` and `KernelEvent::Read`; `marv_mesh::chat_read` (the tool's spec,
the reply, `marv/read` at the door, the reader's `read_chat`); `marv_session::chat_read` (`ChatReadTool`,
`snapshot_of`, `SessionSource`); the TUI and Marv desktop's resident chats (`ResidentSource`: loaded session, else the
saved one; the Chat record's `private`); `marv mesh allow --read`; guards `tests/guards/tests/chat_read.rs`. Where the
build is more conservative than the text above, and why:
- **No rule: refused, never asked.** "Without a rule it asks once" is not built: the target's host always requires a
  covering read rule (both ends read the same rules file), so a reader-side yes could never be served, and a read
  outside a rule the person made isn't allowed. The tool says how the person makes one. `declined` stays a reason in
  the log's vocabulary, unused for now. **Open question** if a one-off, person-approved read is wanted later: it needs
  the target's person asked too (as a cross-process wire is).
- **Busy is any running turn**, not only mid-approval, and it refuses rather than waits.
- **Mesh messages are not served.** They are in the model's conversation only, never the transcript the person saw,
  which is what a read serves; so nothing another agent sent is passed on.
- **Tool lines are names only** (`{"tools": ["read_file", "bash"]}`), not `[edited src/auth.rs]`: a path is a tool
  input.
- **One JSON object a line** (`{"person": …}`, `{"assistant": …}`, `{"tools": […]}`, `{"card": …}`, `{"summary": …}`),
  as the card's words are JSON strings, so a quote can't fake a turn.
- **More refusal reasons:** `limit` (the rate) and `unavailable` (not reachable, not answering as itself, no source:
  `marv -p`, the marv-web server, an older Marv), besides the listed five.
- **`marv mesh allow --read` makes the read rule alone** (no send); the desktop's box adds a read rule next to the
  send rule it makes (same folder, same "lasts").
- **Private is the desktop's only.** A TUI chat has no record to mark, so it's readable under a read rule while it
  runs. **Open question:** a TUI toggle (a `/private` command, or config).
- **The compaction summary is the model's words** about everything it saw, tool outputs included (R4 as decided). It is
  redacted like the rest, and the reader is tainted; noted as the residual risk R1 doesn't cover.
- **Limits on both ends:** the reader's Marv counts its own reads (all targets), each target its readers. The token is
  taken first, so refused reads count too, and past the rate one refusal a minute per reader is logged.
- **No read-and-relay (review P1).** A send rule can be wider than a read rule (`~/Projects` against
  `~/Projects/acme`): a chat that may read could pass what it read on, over D2's no-ask send, to a Marv the read rule
  doesn't cover. So (a) `chat_read` runs only in a turn the person started: in a turn a mesh message or the radio woke
  it is refused ("chat_read runs only in a turn you started"), so another agent can't ask for a read to relay; and (b)
  once a read is in the conversation (`TaintState::read_chat`, which lasts as the taint does: through compaction,
  saved with the session, restored on resume), its `mesh_send`s lose the rule's no-ask and ask as tainted egress does,
  the approval saying the conversation read another chat.
- **A long compaction summary is cut** (on a char boundary, to half a reply, with "(summary cut)"), not a reason to
  refuse a summary read.

### 9.3 Guard tests (`tests/guards/tests/chat_read.rs`)

1. A `send` rule alone doesn't allow `chat_read`; a `read` rule does, and only within its folders.
2. Tool outputs and tool inputs never appear in a served reply (a fixture chat with a secret in a `read_file`
   result; the secret must not appear).
3. A private chat refuses every read, also under a covering rule.
4. The reader is tainted after a read; a tainted reader's egress outside the mesh still asks.
5. Refused reads are in the event log; the log never holds served text.
6. `marv -p` and subagents have no `chat_read`.
7. The card holds only person-typed text: a woken turn's or a tool's text never becomes `title` or `now`.

### 9.4 Decisions (made 2026-10-19, all as recommended)

| # | Decision |
|---|---|
| R1 | Tool outputs are never served by a read (serving them redacted leaks too much). |
| R2 | `read` is its own rule mode, off by default even where `send` is on. |
| R3 | A per-chat "private" toggle: chats are readable under a read rule unless marked private. |
| R4 | The summary needs no model call on the target: card, stored compaction summary, last turns. |
| R5 | A terminal Marv answers reads only while running; nobody reads another process's session files by path. |
| Card taint | Option B: card words are fenced, not tainting (9.1). |
| Visibility | A notice in the read chat plus the event log; no badge. |
| Team board | Deferred: shared memory between chats waits for M2 (who may write, under which grant) and M8 (how it's tainted). Tracked as its own issue. |

---

## 10. Services: chats that work while you're away (M2, #10)

Decided 2026-10-19 (all as recommended; issue #10). **The problem:** a turn the person didn't start (a rule's message woke it) runs with automatic
approval off (`ToolContext::no_auto_approval`, `session.rs:1665`): every edit and every command asks `once` and waits.
In a background desktop chat nobody sees the question, nothing tells the person, and a team of chats stops at its first
edit (live test item 1, 2026-10-18). **The model (§0.7.1): the phone.** An app works in the background under
permissions the owner granted once, scoped and revocable, and the phone notifies the owner when it needs them or is
done.

### 10.1 What exists to build on

- **The ceiling is already defined.** Yolo runs, without asking, exactly the calls a tool vouches for (`auto_safe`):
  a sandboxed bash command with no network and no git write (`bash.rs:458`), a file edit or write outside `.git`
  (`edit_file.rs:181`, `write_file.rs:128`), a worktree subagent (`agent.rs:108`), a send on an existing wire
  (`mesh.rs:1016`). Everything else still asks in yolo: protected paths, home-folder writes, tainted egress, MCP calls,
  host commands. Each call is checkpointed first, so `/undo` puts it back.
- **A woken turn is always tainted** (mesh text taints before its first request), so its egress asks whatever we
  decide here. That is the NX bit (§0.3.1, M8) and this section doesn't touch it.
- **Rules** (`policies.json`, 2b) say who may *talk* to whom. They say nothing about what a chat may *do*.

### 10.2 The grant (decisions S1–S4)

A **service grant** is the person's one-time permission for a chat to work while unattended. In the phone's terms it is
"allow background activity" plus the permissions that come with it.

- **S1. Per chat.** A grant names one chat (its chat id), as a phone's permission names one app. Not per folder: a
  folder's future chats would inherit power nobody looked at. The rule dialog ("Let chats in this folder talk to each
  other") gets a second checkbox, "…and keep working while I'm away", which makes one grant per chat covered *now*.
  Later chats in the folder ask for their own grant.
- **S2. What it allows: yolo's set, and nothing past it.** In an unattended turn under a live grant, a call whose tool
  says `auto_safe`, and that isn't `must_ask` (tainted egress, protected path) or a home-folder write, runs as yolo
  would run it: confined, checkpointed, logged. That is the same registry branch (`registry.rs:255`) with
  `ctx.no_auto_approval` replaced, for that one test, by "no grant". Remembered "always" answers, approved URL
  prefixes and config-seeded scopes still don't apply unattended. So the worst a hijacked service can do is what a
  hijacked yolo chat can do today: change files in its folder (undoable) and run sandboxed commands with no network.
- **S3. What still waits for the person:** everything else, asked `once` as today. The difference is that the person
  now hears about it (10.3) and the chat shows "waiting for you" in the sidebar.
- **S4. Limits on every grant:**
  - **expiry:** until revoked (default), 1 hour or until Marv quits, one click away (as D6 for rules);
  - **spend:** a cap in dollars a day, default $2, from the turns' own usage cost (plan turns count tokens, since a plan
    has no price). At the cap the grant pauses until the next day or the person raises it, and the person is told;
  - **wakes:** unchanged (3 a minute per wire, 10 in 10 minutes then paused, the hop stop). A grant never lifts a wake
    limit. A person's typing still resets the chain.

### 10.3 Notifications (decision S5; the M6 start)

The phone half. Marv desktop sends an OS notification (Tauri's notification plugin; `notify-send` underneath on
Linux) when a chat the person isn't looking at:

1. **needs them:** an approval is waiting ("acme-api needs you: run `cargo publish`?"). Click opens the chat on the
   question. The text is Marv's fixed words plus the approval's title, never the model's prose;
2. **finished:** an unattended turn ended, or a person's turn ended while they were in another chat (the case from
   2026-10-18, chat projects-982821be). One per chat per turn, folded into one notification if several end within
   a few seconds;
3. **paused:** a grant hit its spend cap or expired mid-task.

A notification is shown to the person only. It never reaches a model, the mesh or the radio, and nothing answers an
approval from it except opening the chat (no "Allow" button on the notification: the full request belongs on screen).

### 10.4 Who can be a service (decision S6)

**Desktop chats first.** They have a resident record, a sidebar and notifications. The TUI follows when it needs it
(a TUI is usually watched; `notify-send` would be its notification). **Never `marv -p`** (no person to ask, no
wakes) and **never a subagent on its own** (a granted turn's worktree subagent runs under its parent's grant, narrower
never wider, as today's `no_auto_approval` inheritance, `subagent.rs:108`).

**What starts an unattended turn:** today only a rule's message. Timers (M6) will be a second source. A grant covers
both. A wake on a wire the person granted by hand, or the radio, runs without the grant (as now), because those
sources aren't the rule-bounded ones.

### 10.5 Shown, revoked, audited (decision S7)

- **Shown:** a badge on the chat's header ("works while you're away · $0.40 of $2 today"); a "Background" list in the
  Mesh panel with every grant: chat, folder, expiry, spend, last run, Revoke. `marv kernel grants` lists them in a
  terminal.
- **Made only by the person**, in the desktop, as rules are (§6.2.2): no tool, mesh message, radio frame, project
  config, env var or flag creates, widens or extends one; `Approver::Grant` is never deserialized.
- **Revoked at once:** a generation bump, as for rules. A turn already running finishes its current call and asks
  for the next.
- **Stored** in `~/.marv/mesh/grants.json` beside `policies.json`, with the same store code (0600, atomic, locked,
  generation, unknown fields refused, never a project's, never a symlink).
- **Audited:** the event log gets `GrantMade`, `GrantRevoked`, `GrantExpired`, `GrantPaused { reason: spend }` and
  `GrantUsed { chat, tool, scope_key, turn }` for every call the grant ran. Tool name and scope key only, never the
  input. The transcript marks each such call "ran while you were away (grant)", and `marv kernel replay` counts them.

### 10.6 Guard tests (`tests/guards/tests/services.rs`)

- `no_grant_means_a_woken_turn_asks_for_everything` (today's behavior, unchanged)
- `a_grant_runs_only_what_yolo_would` (bash with network, a git write, a protected path, a home write, MCP and a host
  command each still ask)
- `tainted_egress_asks_under_a_grant`
- `a_grant_ignores_remembered_always_and_url_prefixes`
- `a_model_a_mesh_message_or_a_project_cant_make_a_grant`
- `a_revoked_expired_or_spent_grant_asks_at_the_next_call`
- `a_grant_never_lifts_a_wake_limit_or_the_hop_stop`
- `a_hand_wired_or_radio_wake_runs_without_the_grant`
- `every_granted_call_is_logged_without_its_input`
- `a_notification_never_reaches_a_model`

### 10.7 Decisions (made 2026-10-19, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| S1 | Unit of a grant | Per chat; the rule dialog can grant the chats it covers now |
| S2 | What it allows unattended | Yolo's `auto_safe` set only: sandboxed no-network commands, edits outside `.git`, worktree subagents; never past it |
| S3 | What still waits | Everything else, `once`, with a notification |
| S4 | Limits | Expiry (until revoked by default); $2 a day spend cap, default; wake limits unchanged |
| S5 | Notifications | Needs you, finished, paused; fixed words, never the model's prose, no answer buttons |
| S6 | Who | Desktop chats now; TUI later; never `marv -p` or a subagent alone; rule wakes and (later) timers |
| S7 | Shown and audited | Header badge, Mesh panel list, `marv kernel grants`; `grants.json`; event log `Grant*` events; transcript marks |

### 10.8 Build order

1. `kernel: Grant and grants.json` (pure, beside `policy.rs`).
2. `tools: a grant answers the yolo test unattended` (`registry.rs:255`; `ToolContext.grant`).
3. `session: unattended turns carry the chat's live grant; spend counted per grant`.
4. `desktop: notifications` (S5). This lands first on its own: it helps even without grants.
5. `desktop: grant in the rule dialog, the header badge, the Background list`.
6. `kernel: Grant* events; replay counts`; `cli: marv kernel grants`.
7. Guards and `docs/SECURITY.md`.

## 11. Memory manager: the context window as RAM (M1, #9)

Decided 2026-10-20 (V1–V7 all as recommended, and #63's seven answers with them; issue #9). **The problem:** the window is measured (`context_length`, the token
meter) but not managed. Compaction is one free-form summary at 85% of the window or at `compactTokens` (default
100k, `config.rs:41`), whichever comes first, and it **throws the model's history away**: `summarize_into` swaps
`History` in place, and the saved session's `conversation` is the summary from then on (`sessions.rs:41`). The
transcript keeps what the person saw, and a trajectory keeps the summary, but nothing on disk keeps what the model
saw. Swapping today is paging out to `/dev/null`. §0.3.3–0.3.4 and §0.7.4 frame the rest: a window is a working set
with no addresses and fading attention, not RAM with random access.

### 11.1 What exists to build on

- **Paging policy, specced:** public #63, compaction v2 (`docs/specs/2026-10-14-compaction-v2.md` on
  `docs/63-66-specs`, awaiting approval as bd marv-cgd.3). It has a structured rolling summary, a verbatim tail
  (`min(15k, 15% of the window)`), overflow recovery at 70/50/35%, and a taint guard. It's the right paging-out
  policy, and this section adopts it rather than redesigning it.
- **Swap for one kind of page already exists:** a cut tool output is saved whole to `~/.marv/tool-output/<session>/`
  and the marker names the file, so the model reads back only the part it needs. The source is kept, so a read-back
  is fenced and taints like the original (`output.rs`, #61). That is demand paging done right, for one case.
- **Disk:** the project's files (read again with `read_file`), `~/.marv/memory` (one-line memories, every change
  approved), saved sessions.
- **The cache rule** (CLAUDE.md): the history only grows, the system prompt stays byte-stable, and compaction is the one
  documented rewrite.

### 11.2 The budget (decision V1)

A chat's **context budget** is the size at which the kernel pages out. It is `min(85% of the window, compactTokens)`,
as today, now named and owned by the kernel:

- **set by the person:** the global default in config, and a per-chat override in the desktop chat settings and on
  `/compact budget <n>`. Never set by the model, a project config or a mesh message (a project could otherwise
  starve or bloat every chat opened in it);
- **below the window on purpose:** the 100k default stays, because RAM fades (§0.3.3). A 1M window filled to 85%
  is a slow, expensive and inattentive chat. Keeping the working set small, not just under the limit, is the goal;
- **shown:** on the chat card and the header meter as "used / budget", not "used / window";
- **a grant (§10) can lower it, never raise it**, the same narrowing rule as everything else a service inherits.

### 11.3 The working set (decision V2)

What stays resident across a page-out, in this order:

1. **pinned, never paged:** the system prompt and tools (byte-stable), the open plan (`plan_text`, as today), the
   session's taint state (it lives outside the history and is never cleared, #63 §5);
2. **the task:** the person's newest message that set the goal, kept **verbatim** in the summary message under its
   own heading. Today it's paraphrased with everything else, and "what was I asked to do" drifts after two rounds;
3. **the verbatim tail** (#63 §3.2): the newest turns, never splitting a call from its result;
4. **the summary** (#63's structured template), plus a **page table** (11.4).

Nothing is chosen by a relevance score in this version. A model-scored working set is a cache miss on every step,
and an injection could choose what's kept. Revisit only with evals.

### 11.4 Paging out: compaction as a deliberate act (decision V3)

- **#63 v2 is the policy.** Approving #63 (its seven answers, all as recommended there) is part of this decision.
- **Swap, not `/dev/null`:** before the history is replaced, the kernel writes the turns it pages out to a **swap
  file**, `~/.marv/sessions/<id>.swap/<n>.jsonl` (`n` = the compaction number; 0600 in 0700 folders, like
  sessions). Each turn keeps its `ChatTurn` bytes and its `Provenance`. The tool-output cleanup's rules (age, total
  size, never a live session's) cover swap too.
- **A page table:** the summary message ends with a fixed, kernel-written list (not model prose): the swap file's
  number and turn range, the files read and written (from tool calls, by path), and the tool-output files kept. The
  model knows what exists to page back in, the way a page table knows a page is on disk.
- **A cost the person sees:** every page-out is a kernel event, `Compacted { chat, n, trigger: budget | overflow |
  person, tokens_before, tokens_after, cost, swap_turns }`, written to the event log (sizes only, never text) and
  counted by `marv kernel replay`. The TUI and desktop notice gains "(42k → 9k, $0.03, 118 turns to swap)".

### 11.5 Paging in: the page fault (decision V4)

A **page fault** is the model noticing it lacks something it once had: "what did the test output say", "which
approach did we rule out". There is no hardware trap. The model either asks for the page or re-does the work, and
re-doing it (#63's repeated reads) is the cost we measure.

- **Served by one tool, `recall`:** it searches and reads **this chat's own** swap files (`query` → matching turns with
  their swap number and index; `n` + `range` → those turns). It's read-only and needs no approval. It's **always
  offered**, answering "nothing paged out yet" until the first page-out: offering it only after one would change the
  tool list mid-session and break the cache.
- **Provenance comes back with the page:** a recalled web or MCP result is fenced and taints, like a tool-output
  read-back (`saved_provenance`). Recalling a turn can't launder untrusted text into trusted text.
- **Only its own process:** reading another chat's history is `chat_read` (§9.2), under a rule. `recall` never takes a
  chat id. A subagent recalls its own swap, not its parent's.
- **Disk pages stay where they are:** the project's files are read again with `read_file`. Memories stay the person's
  approved one-liners. `recall` doesn't search either.

### 11.6 Virtual context (decision V5)

**No transparent mapping.** A window has no addresses, so the kernel can't fault a page in behind the model's back.
Every way past the window is an explicit tool call the model makes, and the result is appended like any other
(`read_file`, tool-output files, `recall`, `chat_read`, `memory`). The kernel's job is to make those pages exist
(swap, the page table) and to keep their provenance. It does not inject "relevant context" into a request on its
own. That would be a cache miss every step and a channel an injection could steer (§0.3.1), and it's the
retrieval-augmented shape §0.7.4 says to weigh. It's weighed here and declined for now.

### 11.7 The prompt cache as a constraint (decision V6)

Cache-safe moves, which are free: appending (every page-in is a tool result appended at the end), and the summary
request (`history + one user message`, #63 §4).
Not cache-safe, so **only at a page-out**: any change to earlier turns, including dropping stale tool results,
shortening old outputs or reordering. They are batched into the one rewrite a compaction already pays for, never done
step by step. The system prompt and tool specs never change mid-session (`recall` is always present, 11.5). Each
page-out costs one cold prefix, and the `Compacted` event's cost field makes that visible.

### 11.8 Guard tests (`tests/guards/tests/memory_manager.rs`)

- `a_page_out_writes_swap_before_replacing_the_history` (a failed swap write fails the compaction and keeps the
  history)
- `swap_keeps_provenance_and_recall_refences_it` (a recalled web result is fenced and taints)
- `taint_survives_compaction` (#63's guard, here as a kernel invariant)
- `recall_reads_only_its_own_chat` (no chat id; a subagent can't read its parent's swap)
- `the_budget_is_set_only_by_the_person` (project config, mesh text and the model can't change it; a grant only
  lowers it)
- `the_tool_list_is_byte_stable_across_a_page_out` (`recall` present before and after)
- `the_page_table_is_kernel_written` (paths and numbers from tool calls, not the summary's text)
- `compacted_events_carry_sizes_never_text`

### 11.9 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| V1 | Budget | `min(85% window, compactTokens)`, default 100k, kernel-owned; person sets global and per chat; grants only lower it; shown as used/budget |
| V2 | Working set | Pinned (system, tools, plan, taint) + the task verbatim + #63 tail + summary + page table; no relevance scoring yet |
| V3 | Paging out | #63 v2 as the policy (approve its 7 answers as recommended); swap files keep paged-out turns with provenance; `Compacted` event with cost |
| V4 | Paging in | One tool, `recall`, own chat's swap only, always offered, read-only, provenance restored |
| V5 | Virtual context | No transparent mapping or auto-injected retrieval; explicit tool calls only |
| V6 | Cache | Append-only between page-outs; every other rewrite batched into the page-out |
| V7 | Where it's built | #63 in the public repo (it's open-core compaction); swap, page table and `recall` public too (no mesh); the budget on the card and the `Compacted` event in the private kernel |

### 11.10 Build order

1. Public #63 M1–M7, as its spec says (overflow detection first: it fixes a real failure today).
2. Public: swap files + the page table (`summarize_into` writes swap before it swaps `History`; cleanup covers it).
3. Public: `recall` (marv-tools; always offered; provenance from swap).
4. Private kernel: the budget (per-chat override, card, meter), the `Compacted` event and replay counts.
5. Guards (11.8) and `docs/SECURITY.md` (swap is a new store holding untrusted text).

**Built (#27, bd marv-svl).** Steps 1–3 and the public guards on feat/compaction-v2 (`docs/SECURITY.md`, "Compaction
and saved pages"; `tests/guards/tests/compaction.rs`). Step 4 and the private guards here:
- **The budget** is `MarvSession::context_budget()` = `min(85% of the window, the chat's budget or compactTokens)`.
  The per-chat one is `State::context_budget`, set only by `Configure::context_budget` (40k `MIN_COMPACT_TOKENS` to
  10M `MAX_CONTEXT_BUDGET`, the cap when the window isn't known), saved in the session file (`contextBudget`, saved at
  once) and checked on resume (a value outside the range is dropped). Compaction (`nearly_full`) and the meter use the
  same `threshold()`. Marv desktop sets it from the chat's settings popover (Configure's `contextBudget`, 0 = default);
  the desktop and the TUI take `/compact budget [<n>|default]` (`commands::compact_budget`: `budget`, any case, alone
  or with one number or `default`; anything else is a summary focus), and Marv says when the context is already past
  a lowered budget (the next message compacts). The `ready`, `usage`, `context_budget` and `context` events carry it.
  No config key: the global default stays `compactTokens` (the person's own config, the desktop's too; a project's
  `.marv/config.json` is never read). A skill named like a built-in desktop command isn't offered.
- **Shown:** the desktop header's meter is the context of the budget: the last request's, or after a compaction the
  compacted history's estimate until the next request (`context_used()`); the chat card carries `context_used` (none
  before a request) and `context_budget` (numbers only; a peer's cut to `MAX_CARD_TOKENS` on receipt and where shown),
  and `mesh_list` shows them as Marv's words with the state ("(context 42k of 100k tokens)"), under the same rule as
  the card's words. A sleeping chat's card has no numbers (its last usage isn't saved).
- **`KernelEvent::Compacted { chat, by: budget | overflow | person, level, tokens_before, tokens_after, cost_usd_micros,
  page, swap_turns }`**: written by `summarize_level` after the new history is installed (and by overflow recovery's
  level 3, which drops turns without a summary: no cost), when the person's Marv gave
  the session the log (`MarvSession::log_kernel_events`: Marv desktop's chats, named by chat id; the TUI on the mesh,
  by its mesh name; never `marv -p`). `tokens_after` is an estimate (the history, system prompt and tool specs at four
  characters a token): no request has measured it yet. `n` from the sketch is `page` (the saved page's number, none
  when no page was written). `marv kernel replay` counts compactions, those an overflow forced and the turns paged.
- **Not built:** the TUI/desktop notice's "(42k → 9k, $0.03, 118 turns to swap)" (the event has the numbers; the
  notice is unchanged), and "a grant can lower it": a grant has no context field yet (an open question for the
  allocator's grants, #32). Guards: `tests/guards/tests/memory_manager.rs`
  (`the_context_budget_is_set_only_by_the_person`, `compacted_events_carry_sizes_never_text`,
  `a_cards_context_numbers_are_numbers_within_limits`).

## 12. Information flow: the kernel's NX bit (M8, #16)

Decided 2026-10-20 (N1–N8 all as recommended; issue #16). **The problem:** a model can't refuse to run data (§0.3.1). Taint,
provenance and fencing are the software NX bit, but they grew per feature: each source picked its own `Provenance`,
and `ToolOutput::ok` defaults to `Project` (`types.rs:493`), so a tool that forgets says "trusted workspace text".
An inventory (2026-10-20) of every way text reaches a model found the core sound (fences are defused, taint is saved,
resumed, inherited and absorbed, and the woken-turn, radio, Herdr and hook paths are guarded) and 14 gaps. The
largest: the GitHub door, network bash and process output are `Project`, so others' text off the network neither
fences nor taints (`github/mod.rs:489`, `bash.rs:668`). A cloned repo's `AGENTS.md`, skills and `.marv/agents`
load with full authority. And compaction drops the fences (`compact.rs:170`).

### 12.1 The invariant (decision N1)

For `docs/SECURITY.md`, one rule above the per-source ones:

> **Authority comes from the person and from Marv's fixed words. Everything else is data.** Every piece of text in a
> model's context has a source. Text from outside the person's own choices is fenced as data, and text that someone
> other than the person could have written over a network or another agent taints the conversation. Data gains
> authority only by passing through the kernel: an approval the person answered, a rule or grant they made, or a
> project they trusted (N3). No other path exists, and a guard fails when one appears (12.6).

### 12.2 Three classes of source (decision N2)

| Class | Sources | Fenced | Taints |
|---|---|---|---|
| **Authority** | the person's typing and answers; Marv's fixed text; the person's own files (`~/.marv/AGENTS.md`, `~/.marv/skills`, personal memory, their config) | no | no |
| **Workspace** | the project folder: files, file names, `git log` of local history, checks, the formatter; a **trusted** project's instruction files (N3) | as today (checks and diagnostics quoted) | no |
| **Outside** | web, MCP results, mesh, radio, Herdr reads, untrusted hooks, images; **new:** the GitHub door, the output of any command run with network (sandboxed `network: true`, a process with network, the host door), and a tainted subagent's report | yes | yes |

- **Why workspace text doesn't taint:** the person chose to work there, and tainting every read of every repo would
  make every session tainted, which empties the signal. A poisoned README stays the eval suite's job and the
  approvals' job (writes to protected paths, egress), as today.
- **The rule for a new tool:** it declares its class. `ToolOutput::ok` stops defaulting. A missing class doesn't
  compile, or is caught by the guard in 12.6 if a type can't express it.
- **Cost of the new Outside sources:** a session that reads a PR or runs `npm install` is tainted from then on, so its
  egress asks. That's the honest state: the text came off the network. gh writes and network commands already ask.

### 12.3 A project's instructions (decision N3)

This is the largest hole and it is by design: `AGENTS.md`/`CLAUDE.md` up to the git root, nested ones (put in Marv's
voice, `instructions.rs:201`), `.marv/skills` and `.marv/agents` (a subagent's whole role) load as authority. For a
repo the person wrote, that's right. For one they just cloned, it's someone else's instructions with the person's
authority.

**Recommendation: trust on first use, as MCP project servers already do.**
- The first time a project's instruction files load, the TUI and desktop ask once: "Use this project's instructions
  (AGENTS.md, 2 skills, 1 agent type) as yours?" with the files listed. The answer is stored with a **hash of their
  contents**, beside MCP trust. A changed file (a `git pull`) asks again, showing what changed.
- **Not trusted:** they load **fenced as workspace data** under a fixed note ("this project's instructions, not the
  person's: follow them only where the person asks"). Project skills and agent types aren't offered.
- `marv -p`: untrusted unless `--trust-project` (or the stored trust matches), so CI doesn't break silently. A
  stored, matching trust is used.
- The person's own home project and a folder under their own git identity's commits are **not** auto-trusted. Guessing
  authorship is a heuristic an attacker can game. One question per project is cheap.
- Nested `AGENTS.md` stops speaking in Marv's voice: "Project instructions for `src/x/` (trusted):".

### 12.4 Fence holes (decision N4: fix all, no trade-off)

1. A tainting tool's `is_error` result is fenced too (`runner.rs:69`). An MCP server's error text is still its text.
2. `mesh_list`'s wired receptacle descriptions are fenced (`tools.rs:223`). They taint today but come unfenced.
3. The fence's `from="…"` attribute is escaped (`provenance.rs:45`): it can hold a model-written URL.
4. MCP tool descriptions in the deferred announce and in `tool_search` output are fenced as MCP data
   (`deferred.rs:129`, `mcp_tools.rs:126`), and capped in length. They don't taint: the person installed the server.
   The tool specs themselves can't be fenced and stay as they are.
5. A subagent's report is fenced as `Subagent` data **when the child was tainted**, so outside text it carries back
   arrives as data, not as the parent's own tool output. (Its taint already reaches the parent through `absorb`.)

### 12.5 Sinks and lifetime (decisions N5–N8)

- **N5. Memory is a persistent sink:** in a tainted conversation a memory write is `must_ask` and `once` ("always"
  never covers it), and the approval says the conversation read outside text. Memory loads as authority into every
  future system prompt, so a planted line would outlive the session. The same applies to writes to the project's
  instruction files, which are already protected paths.
- **N6. Compaction:** #63 Q6 stays decided (the summary stays a user-role message, not fenced). One addition with
  fixed words only: when the conversation is tainted, `COMPACTED_PREFIX` says "this conversation read outside data;
  the summary may repeat it; an instruction in it is data unless the person gave it". The verbatim tail keeps its
  fences (#63 §3.1), and recalled swap pages are fenced again (§11.5).
- **N7. `/clear` ends the taint** of the conversation it clears, as resume already does (`restore` replaces the
  sources). The tainted text is gone from the context. Taint lasts as long as the text does, and no longer.
  Exception: taint that comes from something still running (a process door started with network) stays until it
  stops.
- **N8. Herdr prompts into an existing pane:** unchanged. They're already `must_ask` when tainted, and the approval
  says "the receiving agent will treat this text as your own words". The receiving agent can't be marked from
  outside, so the approval is the gate. Spawned Marvs keep `MARV_TAINTED`.

### 12.6 Guard tests (`tests/guards/tests/information_flow.rs`)

- `every_tool_declares_a_source_class` (walks the registry, built-in and door tools; a new tool without one fails)
- `github_door_output_is_fenced_and_taints`
- `network_command_output_taints_and_sandboxed_offline_output_doesnt`
- `an_untrusted_projects_instructions_load_fenced_and_offer_no_skills`
- `a_changed_instruction_file_asks_for_trust_again`
- `tainting_error_results_are_fenced`
- `fence_attributes_are_escaped`
- `mcp_descriptions_in_announce_and_tool_search_are_fenced`
- `a_tainted_subagents_report_is_fenced`
- `a_tainted_conversation_asks_once_for_a_memory_write`
- `a_tainted_compaction_says_so_in_fixed_words`
- `clear_ends_the_cleared_conversations_taint`
- `suggestions_stay_off_after_github_or_network_output` (follows from N2)

### 12.7 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| N1 | The invariant | 12.1, as the first rule of `docs/SECURITY.md`'s provenance section |
| N2 | Classes | Authority / Workspace / Outside; GitHub door, network-command output and host-door output move to Outside; tools must declare a class |
| N3 | Project instructions | Trust on first use, content-hashed, beside MCP trust; untrusted loads fenced, no project skills or agents; `marv -p --trust-project` |
| N4 | Fence holes | Fix all five (12.4) |
| N5 | Persistent sinks | Memory writes in a tainted conversation ask `once` |
| N6 | Compaction | #63 Q6 as decided, plus a fixed-words note when tainted |
| N7 | Taint lifetime | `/clear` ends it; a networked process keeps it while it runs |
| N8 | Existing panes | Unchanged; the approval is the gate |

### 12.8 Build order

1. Fence holes N4 (small, no behavior choice).
2. Source classes N2: the declared class, GitHub and network output to Outside.
3. Memory sink N5, `/clear` N7, the compaction note N6.
4. Project trust N3 (the largest: storage, the TUI and desktop prompts, `--trust-project`).
5. Guards 12.6 and `docs/SECURITY.md` (the invariant first).

## 13. The system-call table: tools as a versioned ABI (M5, #13)

Decided 2026-10-20 (C1–C6 all as recommended; issue #13). **The problem:** tools are the system calls, and their properties
live on the `Tool` trait (`types.rs:517`): kind, provenance, network, auto_safe, protected targets, scope,
own_marvs_only. That's the right place, but nobody can see them in one table. Two of them **fail open by default**:
`kind()` defaults to `Read`, which runs without asking (`types.rs:526`), and `provenance()` defaults to `Project`,
which never fences or taints (`types.rs:530`; §12's gap list starts there). Which calls a mode gets is decided by
scattered booleans: `no_auto_approval`, the `doors` flags, the subagent's filtered list (`subagent.rs:342`), and
`tool_specs_for`'s three switches.

### 13.1 One table, generated from the code (decision C1)

- **The trait stays the source of truth.** A hand-kept table drifts. `cargo test` generates `docs/SYSCALLS.md` from
  the registry (built-in tools, door tools, mesh tools, with MCP as one row per class) and fails when the checked-in
  file differs, like the `mesh_list` golden (3a). A tool change shows up in review as a table diff.
- **Columns:** name · kind · source class (§12.2) · may reach the network · checkpoints · yolo may run it
  (`auto_safe` can say yes) · the floor (never asks / asks / asks every time / refused) per mode (13.2) · `must_ask`
  triggers (taint, protected path) · its "always" scope.
- `marv kernel syscalls` prints the same table for the running build.

### 13.2 Modes: one value, set by the kernel (decision C2)

A call runs in exactly one **mode**, which the kernel sets on `ToolContext` and nothing else can change:

| Mode | Started by | Today's flags it replaces |
|---|---|---|
| `Person` | the person typed | (default) |
| `Woken` | a rule's message, no grant | `no_auto_approval` |
| `Service` | a rule's message or a timer, under a grant (§10) | `no_auto_approval` + `grant` |
| `Radio` | a radio message | `no_auto_approval` |
| `Headless` | `marv -p`, the SDK | the headless approver's flags |
| `Subagent { parent }` | a parent's `agent` call | the subagent's filtered list, `for_subagent` |

**One function, `syscall_floor(tool, input, mode) -> Floor`,** answers both "is it offered" and "what's the least it
must ask", and `run_tool` calls it. Then taint, protected paths and D2 apply as today. A floor can only be raised by
the later checks, never lowered. The table in 13.1 is that function, printed. The booleans move behind it in one PR,
with no behavior change (a golden of today's answers per tool and mode proves that).

### 13.3 The single check (decision C3)

`run_tool` (`registry.rs`) is already the entry for built-in, door, MCP and mesh tools, and for subagents. It becomes
the **only** one: a scan guard fails if any code outside `run_tool` and its tests calls a tool's `run`. §4.3 did the
same for the mesh's `authorize`. The order is fixed and documented: unknown tool → parse and repair → `check` →
floor (13.2) → protected path → taint/`must_ask` → D2 → yolo/`auto_safe` → remembered scopes → ask → checkpoint →
run → class → fence → mark taint.

### 13.4 Fail closed by default (decision C4)

`kind()` and the source class (§12, N2) lose their defaults: every tool says what it is. A new tool that forgets
doesn't compile. `auto_safe` keeps its default (`false`, already closed). MCP tools get theirs from the MCP adapter
(kind `Egress` once tainted, class Outside), as now.

### 13.5 Versioning: the ABI (decision C5)

What the model and saved sessions depend on is a tool's **name** and **schema**:
- **Names are never reused or renamed.** A removed tool leaves a tombstone: a call to it in a resumed session gets
  "`<name>` was removed in Marv X.Y" instead of today's "Unknown tool" plus the list (`registry.rs:137`). That list
  invites the model to retry under another name.
- **Schemas change additively:** a new optional field is fine, while removing, renaming or narrowing one is a new
  tool. A golden of every built-in spec's bytes (`tests/guards/tests/syscall_abi.rs`) fails on any change, so a
  change is always deliberate and noted in the changelog.
- **Within a session, specs are byte-stable** (the cache rule, as today). Across versions, a resumed session sends the
  new specs once: one cold prefix, the same as after compaction.
- No per-tool version numbers. The golden and tombstones give the guarantee without a field the model must read.

### 13.6 The open bus (decision C6, §0.7.2)

Third-party system calls come only through **MCP**, the open bus. There is no native plugin ABI: a crate someone
else writes never runs inside Marv's process with the trait's power. Each MCP tool appears in the table under its
server, with the adapter's fixed properties. A server can't declare itself `Read`, `auto_safe` or Workspace-class:
`readOnlyHint` stays advisory, and a tainted session still asks.

### 13.7 Guard tests

- `syscalls_md_matches_the_registry` (the generated table)
- `syscall_abi.rs::builtin_specs_match_the_golden`
- `only_run_tool_runs_a_tool` (scan)
- `the_floor_matches_todays_answers_per_mode` (the no-change golden for 13.2)
- `a_removed_tool_answers_with_its_tombstone`
- `an_mcp_tool_cant_declare_itself_safe`

### 13.8 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| C1 | The table | Generated from the trait by a test into `docs/SYSCALLS.md`; `marv kernel syscalls` |
| C2 | Modes | One `CallMode` on `ToolContext`, set by the kernel; `syscall_floor(tool, input, mode)` replaces the booleans, proven by a no-change golden |
| C3 | The single check | `run_tool` only, scan-guarded; the order in 13.3 documented |
| C4 | Defaults | `kind` and the source class required; nothing fails open |
| C5 | Versioning | Names never reused, tombstones for removed tools, additive schemas, a spec golden; no version numbers |
| C6 | Third parties | MCP only; adapter-fixed properties; hints advisory |

### 13.9 Build order

C4 goes first, landing with §12 step 2 (the class) in the same public hardening work (#28). Then, all public: the
generated table (C1), the spec golden and tombstones (C5), `CallMode` with the no-change golden (C2), and the scan
guard (C3). `marv kernel syscalls` is private (the kernel CLI).

## 14. Memory protection between processes (M11, #19)

Decided 2026-10-20 (P1–P6 all as recommended; issue #19). **The problem:** two chats in one folder write the same files and
nothing stops either. `with_file_lock` (`file_lock.rs`) serializes read-change-write **within one process** only:
desktop chats share it, separate TUIs don't. Nothing notices that a file changed since a chat read it, so an
`edit_file` or `write_file` built on an old read silently drops another chat's change, or the person's change made
in their editor. Services (§10) make this common: unattended chats editing while nobody watches. §8.1's answer
(declared read and write sets, Bernstein's test at admit) is sound but asks for declarations nobody will write, and
it can't see bash.

### 14.1 Separate address spaces: worktrees (decision P1)

The OS answer to "two processes, one memory" is separate address spaces, and Marv has them: git worktrees (the
`agent` tool's `isolation: "worktree"`, Herdr spawns). **Recommendation:** the rule dialog's "keep working while I'm
away" (§10.2 S1) gains "each in its own worktree", **on by default when two or more granted chats share a folder**.
The person merges the branches, as for a worktree subagent. Chats the person drives stay in the folder as today.
Merging becomes the explicit, reviewed moment where changes meet.

### 14.2 Stale writes refused: compare-and-swap (decision P2)

For chats that do share a folder, the cheap, exact check, which works across processes:
- each chat remembers a **content hash per file** it read (`read_file`, a `grep` hit doesn't count) or wrote;
- `edit_file` and `write_file` on an **existing** file check the current hash against the chat's. On a mismatch the
  call is refused before anyone is asked: "`src/api.rs` changed since you read it (by acme-api-3f2a91c0 / outside
  Marv). Read it again." The writer is named from the registry's recent writes (14.4) when known;
- an existing file the chat never read is refused the same way ("read it first"). A new file is fine;
- the hash is checked and the file written **inside a short cross-process lock** (P3), so two writers can't both pass
  the check;
- this helps even a single chat: the person's own edit in an editor is no longer overwritten.

### 14.3 Locks: short, never held across steps (decision P3)

`with_file_lock` gains a cross-process half: an advisory lock (`flock`) on a per-path file under the runtime folder
(`marv_sandbox::runtime_dir`, already empty in every sandbox), held only for check-hash + write. No lock is held
across a model step or a turn. A model that "takes a lock" and then waits or dies is the deadlock every OS learned to
avoid, so locks the model can take aren't offered (an advisory claim on the team board, #22, is the coordination
tool instead).

### 14.4 Conflicts: shown, not scheduled (decision P4)

- **No declared R/W sets and no admit test now.** §8.1 stays plan-only. The kernel uses **observed** write sets: the
  paths each chat's tools reported changing (the checkpoint chain already records them per turn,
  `sessions.rs:52`), kept in the registry for the last 30 minutes.
- **Shown to the person:** when a chat writes a path another live chat in the folder wrote in that window, the chat
  card and Mesh panel show "also editing here: <chat> (2 files)". Paths only, never content, and in the event log as
  `WriteOverlap { a, b, paths_count }`.
- **Shown to rule-covered peers:** `mesh_list` lists a covered peer's recent write paths, so a team can see who's
  touching what. Paths are workspace data.
- bash's writes aren't seen per call (the sandbox doesn't report them). The next `edit_file` on such a file still hits
  P2. The overlap list covers the tools that report paths. That's an accepted gap.

### 14.5 How it composes (decision P5)

- **Services (§10):** P1's worktree default applies; a grant's edits are still checkpointed and undoable, and P2
  refusals in an unattended turn are just a failed call (the model re-reads), never an approval waiting.
- **Shared memory (#22):** the board is where a team says "I'm on `src/api`", advisory and fenced. P2 is the hard
  guarantee underneath it.
- **Subagents:** a shared-folder subagent has its own hash map. Its parent's P2 check catches the subagent's writes
  like anyone else's, which fixes a parent overwriting its own subagent's edit.
- **`/undo`:** restoring a checkpoint is the person's act. It updates the chat's hashes, and other chats get P2 on
  their next write.

### 14.6 Guard tests (`tests/guards/tests/memory_protection.rs`)

- `an_edit_after_another_chats_write_is_refused_until_reread` (two sessions, two processes)
- `an_existing_file_never_read_cant_be_overwritten`
- `the_check_and_write_are_atomic_across_processes`
- `no_lock_outlives_one_call`
- `overlaps_are_shown_as_paths_never_content`
- `granted_chats_sharing_a_folder_default_to_worktrees`

### 14.7 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| P1 | Address spaces | Worktrees; on by default for 2+ granted chats in one folder; person-driven chats unchanged |
| P2 | Stale writes | Per-chat content hashes; edit/write of an existing file refused unless current; never-read refused |
| P3 | Locks | Short cross-process `flock` around check+write only; no model-held locks |
| P4 | Conflicts | Observed write sets (30 min), shown on card, Mesh panel, peers' `mesh_list`, event log; no admit test; §8.1 stays plan-only |
| P5 | Composition | As 14.5 |
| P6 | Public or private | P2 and P3 public (a correctness fix for every Marv); P1, P4 private kernel |

### 14.8 Build order

1. Public: P2 + P3 (marv-tools: hash map in `ToolContext`, the check in `edit_file`/`write_file`, flock in
   `file_lock.rs`). Ship first: it fixes the single-chat editor case today.
2. Private: observed write sets in the registry, `WriteOverlap`, card and Mesh panel, `mesh_list` paths.
3. Private: the worktree default in the rule dialog (after #24 lands the grant UI).
4. Guards.

**Built (#30, bd marv-1rb).** Step 1 (public): `marv_tools::file_versions` and the cross-process half of
`file_lock`. Steps 2–4 (private):
- **Observed write sets:** `marv_kernel::writes` (`WriteSet`, 30 minutes, at most 200 paths, cleaned on receipt),
  `Entry::writes`, `Registry::overlaps`, `KernelEvent::WriteOverlap { a, b, paths_count }`. The edit tools tell
  `ToolContext::writes` (`marv_tools::WriteObserver`) each file they wrote, after the write and after the lock; the
  session's observer (`marv_session::mesh::MeshWrites`) gives the real path to `Switchboard::note_write`, which feeds the
  door's `marv/info` (`PeerInfo::writes`) and the registry (`MeshRegistry::wrote`, which logs an overlap when a pair's
  shared paths grow, never per write). Shown on the desktop chat's header ("also editing here: <chat> (2 files)", paths
  in its tooltip; only this desktop's own chats by title, any other Marv by its mesh name), in the Mesh panel
  ("editing the same files"), and as an `editing:` line in `mesh_list` for a peer a rule lets send to the caller (the
  text flows from it; `Switchboard::rule_covers_from`). That line **taints** the reader (`tools::WRITES_TAINT`,
  decided in review: the names are another model's, and a peer can say any path) and shows only plain relative paths
  inside the peer's folders (`[A-Za-z0-9._-]` components, at most 100 bytes, 5 and "and N more"). A door publishes its
  write set only with `mesh.cards` on and never for a private chat; a received set keeps only paths inside the folders
  the peer says it works in (`cwd`, and a worktree chat's `work_dir`). The memory files never enter a write set. The
  checkpoint chain's per-turn paths (`sessions.rs`) weren't used: they are project-relative and kept per turn, not per
  chat in time.
- **Worktree default:** `marv_kernel::worktree_by_default`; the Mesh panel's "keep working while I'm away" gains
  "…each in its own worktree" (on by default). Granting chats that share a folder (the same, or one inside the other;
  counting chats granted before) puts each one **without a conversation yet** in a new worktree
  (`~/.marv/worktrees/<project>/<branch>`, `marv_session::worktree::create_worktree`): the chat record's `worktree`, its
  session's root, and its grant's folder. It stays listed, named and rule-covered from its folder. A chat whose
  conversation is already saved in the folder, or whose session is loaded there now, **stays** (a saved session belongs
  to its folder) and the person is told; outside a git repository it stays too, said. All or nothing: the worktrees
  are recorded only once every grant is made, and taken back (folder, branch) if one fails. When the grant ends the
  chat keeps its worktree and its conversation there (only its unattended runs stop; decided 2026-10-22). The record
  is cleared (`git worktree prune`) only when the chat is deleted or the person picks "Move back to the folder" in the
  chat's settings (`move_chat_to_folder`: a new conversation in the folder; the branch and worktree are kept, and the
  answer says whether the branch has work not merged or committed yet). A worktree whose folder is gone falls back to
  the folder the same way, told in the chat. The person merges the branch; nothing commits or removes it for them. A desktop chat keeps
  its file versions (§14.2) while it sleeps (`Resident::file_versions`).
- **Guards:** `tests/guards/tests/memory_protection.rs`, the six of 14.6 and `a_peers_write_set_is_cleaned_on_receipt`;
  Marv desktop's `granted_chats_get_their_worktree_as_root_and_grant_folder` (a real repository).
- **Open:** a P2 refusal doesn't name the other writer yet ("by acme-api-3f2a91c0"): the registry knows recent writers,
  but the public tool has no view of it.

## 15. Interrupts, signals and timers (M6, #14)

Decided 2026-10-20 (I1–I7 all as recommended; issue #14). **The problem:** each event that starts, stops or changes a turn has its
own path: Esc cancels the stream; a person's message typed during a turn queues behind it (the TUI's gate, the
desktop's chat); mail is read between steps and wakes an idle chat under the wake limits (§5.5); the hop and rounds
caps stop chains; the scheduler queues wakes when K slots are full (§5.3). Grants will add pauses and expiries (§10),
and nothing wakes a chat at a time. The phone (§0.7.1) has one answer for all of these: events go to the OS, the OS
decides what interrupts what, and the person is notified.

**Status: built** on `feat/kernel-interrupts` (private; steps 2–5 after the public "send now"):
- `marv_kernel::interrupt`: `Source` (its order is the queue's priority), `Kind`, `preempts`/`handling` (I2: preempt only
  the person's stop or close, a grant revoked or expired or the deadline in an unattended turn, shutdown),
  `InterruptQueue`, `SignalKind`, `SignalBy::Person` (the only variant). `marv_kernel::timer`: `Timer`, `TimerStore`
  (`~/.marv/kernel/timers.json`, the grants store's rules), `MAX_AHEAD_MS`, `SELF_TIMERS_PER_DAY`, `TURN_DEADLINE_MS`.
  Events `Interrupt`, `Signal`, `TimerMade` (`fires_at_ms`: `at_ms` is the line's own stamp), `TimerFired`,
  `TimerVoid`; `replay` counts them.
- `marv_session`: `MarvSession::wake_timer` (`Woken::Timer`: carries the grant as a rule's wake does, reads only
  rules' mail, never the radio), `halt_at_step` (the pause: checked after `after_step`, so the step's calls all have
  their results and no next request goes out), `timers::WakeMeTool`/`FileSelfTimers` (`SessionInit::timers`, offered
  in every desktop chat, granted or not, after `chat_read`). `ChatGrant::expired` (default false).
- `marv_web::host`: `run_person`, `run_timer`, `with_deadline`.
- Marv desktop (`app/src-tauri/src/chats.rs`): the resident record holds the `InterruptQueue` (the person's
  messages, fired timers; mail stays in its inbox), `paused`, `unattended`. The watcher's `look` takes the person's
  message first, then (unless paused) an admitted wake, mail, a timer. `Chats::signal`/`stop_team`/`tick`/`fire`/
  `grant_revoked`; Tauri `chat_signal`, `stop_team`, `add_timer`, `remove_timer`; a 10 s kernel tick. The Mesh
  panel: pause/resume/stop and timers per grant row, "stop the team" per rule row.
- **Deviations and choices (after review):** the deadline (a constant 30 minutes, not yet a per-grant field) applies
  only to a turn running under a live grant (`turn_deadline`): an ungranted woken turn asks for everything and is never
  paused by it. Hitting it stops the turn and pauses the chat; a stopped turn that doesn't end within 30 s has its
  session unloaded (`Deadline::Stuck`). The person's send-now into an unattended turn makes it theirs (no deadline, no
  stop for its grant's end). "Stop the team" pauses and stops every covered chat; "resume the team", the sidebar's
  resume and the person's own message resume. A pause holds wakes and timers, not the person's messages, and holds
  across a wake that starts as it lands (`halt_at_step` is cleared only by a resume or the person's turn). Timers: a
  due timer for a paused chat, or one already queued, stays due in the file (queued at most once); it is marked fired
  only when its turn starts, after a last check that the grant holds (`may_fire`: a chat's own timer only under the
  grant it was set under); a chat not on the mesh voids it (`NotOnMesh`). A `wake_me` timer keeps the setting turn's
  mesh hop and its turn starts there; timer turns count against the rule wakes' overall limit; `wake_me` asks
  (`must_ask`) in a tainted session; its limit is a rolling 24 hours. Recurring: every N minutes (15 min to 31 days),
  or daily at a local time in the person's IANA zone (`Repeat::Daily`, chrono-tz: a skipped time fires at the first
  valid minute after it, a repeated one at the earlier instant). A person's recurring timer's words reach the model as
  theirs (under a fixed line); only a chat's own note is fenced. Interrupts at a step boundary add no text of their own,
  so they never make two user turns in a row.
- Guards: `tests/guards/tests/interrupts.rs` (15.6, each proven by a revert).

### 15.1 One model (decision I1)

Every such event is an **interrupt** with a source and a kind, delivered to a chat through the kernel (the resident
record on the desktop, the bridge in the TUI) and nowhere else:

| Source | Kinds |
|---|---|
| Person | message, stop (Esc), approval answer, close |
| Kernel | grant revoked / expired / spend cap, page-out (§11), unload (§5.3), shutdown |
| Mail | a rule's wire, a hand-made wire |
| Radio | a radio message |
| Timer | a timer fired (15.4) |

### 15.2 Priorities and preemption (decision I2)

- **Preempt now** (at the next safe point: the stream is cancelled, a running tool call finishes or is killed as
  today): the person's stop, a grant revoked or expired mid-turn in an unattended turn, shutdown. Nothing else
  preempts a running turn.
- **At the next step boundary** (appended, cache-safe): mail and radio on the turn's own wires (as today), and the
  pause from a spend cap (the current step finishes, no next one).
- **Queued as the next turn**, in this order: person > kernel > mail > radio > timer. A person's message always goes
  ahead of any queued wake, and a timer never jumps a person.
- **New: "send now"** for the person (Alt+Enter in the TUI, a button in the desktop composer): the message is
  delivered at the next step boundary of the running turn, as a person's message, instead of after it. Plain Enter
  keeps today's queueing.

### 15.3 Signals between chats (decision I3)

A chat can receive **stop, pause, wake, mail and close**. **A peer can send only mail** (and, under a rule, the wake
that mail causes, inside the wake limits). No chat can stop, pause or close another. There's no `kill(2)` between
processes, so a hijacked chat can't take its team down, and a lead that wants a worker to stop asks it by mail. The
person can send every signal, from the Mesh panel per chat or for all chats under a rule ("stop the team").

### 15.4 Timers (decision I4)

- **Recurring timers are the person's:** made in the desktop ("check CI every morning at 9 in acme-api"), needing a
  live grant (§10) for the chat, shown in the Background list with the grant. No tool, project or mesh message makes
  one.
- **One-shot self-timers under a grant:** a granted chat may call `wake_me { after, note }` ("in 20 minutes, check the
  CI run"): at most 6 hours ahead, at most 10 a day per chat, inside the grant's spend cap and wake limits, and void if
  the grant ends. The note is the chat's own text, delivered fenced under a fixed line ("A timer you set fired:"), never
  as the person's words. Outside a grant, a call is refused with "needs a background grant". The tool is still
  offered everywhere, so the tool list stays stable (§13's floor).
- **Deadline per unattended turn:** 30 minutes of wall time by default (on the grant), then the kernel stops the turn
  and notifies "paused: ran 30 min".
- **Where timers live:** `~/.marv/kernel/timers.json` (the grants store's rules: 0600, atomic, person-made only for
  recurring). They fire only while Marv desktop runs. There's no system daemon (launchd/systemd) in this phase. A
  timer missed while Marv was closed fires once at the next start, marked late. Several missed recurring fires
  collapse to one.

### 15.5 What reaches the event log and the person (decisions I5, I6)

- **Event log:** `Interrupt { chat, source, kind, action: preempted | at_step | queued | dropped }`, `TimerMade`,
  `TimerFired { late }`, `TimerVoid { reason }`, `Signal { to, kind, by: person }`. Ids and fixed words, never text.
  `marv kernel replay` counts them.
- **The person:** the outward interrupt is the notification (§10.3). A fired timer that runs and finishes notifies like
  any unattended turn. A timer that can't run (grant ended, spend cap) notifies once.
- **Shells:** desktop first (timers, signals per chat, stop-the-team). The TUI keeps Esc and mail, and gets "send
  now". `marv -p` has no timers.

### 15.6 Guard tests (`tests/guards/tests/interrupts.rs`)

- `only_stop_revoke_and_shutdown_preempt_a_turn`
- `a_person_message_always_runs_before_a_queued_wake`
- `a_peer_can_send_only_mail` (no stop, pause or close over the mesh)
- `wake_me_needs_a_grant_and_stays_inside_its_limits`
- `a_recurring_timer_is_made_only_by_the_person`
- `a_timer_note_arrives_fenced_never_as_the_persons_words`
- `an_unattended_turn_stops_at_its_deadline`
- `interrupt_events_carry_no_text`

### 15.7 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| I1 | One model | Interrupts with source and kind, through the kernel only (15.1) |
| I2 | Priorities | Preempt: stop, revoke/expire, shutdown. Step boundary: mail, radio, spend pause. Queue: person > kernel > mail > radio > timer. New "send now" |
| I3 | Signals | Peers send only mail; only the person stops, pauses or closes; "stop the team" |
| I4 | Timers | Recurring = person-made, under a grant; one-shot `wake_me` under a grant (≤6 h, ≤10/day, fenced note); 30 min deadline per unattended turn; desktop-only, no system daemon; missed fire once, late |
| I5 | Audit | `Interrupt`, `Timer*`, `Signal` events, no text; notifications per §10.3 |
| I6 | Shells | Desktop first; TUI keeps Esc and mail and gets "send now"; no timers in `marv -p` |
| I7 | Public or private | "send now" public; everything else private kernel |

### 15.8 Build order (after #24's grants)

1. Public: "send now" (TUI and desktop composer).
2. Private: the interrupt queue on the resident record (one ordering, no behavior change except priorities), events.
3. Private: signals in the Mesh panel, stop-the-team.
4. Private: turn deadline on grants; `wake_me`; recurring timers and `timers.json`; missed fires.
5. Guards.

## 16. The allocator: several cores and their accounting (M3 #11, M4 #12)

Decided 2026-10-20 (A1–A9 all as recommended; issues #11, #12). M3 and M4 are one design (§0.7.4): which core does what work, at
what price, under what budget. **What exists:** one model per chat, picked by the person (`/model`, the desktop
picker). An agent type's `model:` picks a model on the **same** provider for a subagent (`agents.rs:53`), which
makes subagents the first second core. Every request's tokens and cost are summed per session (`usage.rs:19`, cost
reported or estimated, `None` for local). A grant has a daily spend cap (§10, #24). Plans (#159) are cores with quotas
the provider enforces, and nothing else caps anything.

### 16.1 Who picks the core (decision A1)

- **The person, per chat**, as today. This is the chat's default core.
- **Per task, through subagents:** an agent type names its core. That's the "big and little" mechanism: a
  `reviewer` or `explorer` type on a cheap fast model, the chat itself on a large one. The person (or an agent type
  they installed) decides what's "hard". There's no classifier.
- **Not per step inside a chat.** Switching the model between steps throws away the prompt cache every switch, changes
  the per-family prompt and tools (#66), and makes a chat's behavior unpredictable. The cost of a cold prefix usually
  exceeds what the cheaper core saves. Revisit only with eval numbers (§0.7.3: the map doesn't justify it).

### 16.2 The little core (decision A2)

One person-set `smallModel` (config, desktop settings) for Marv's own side jobs: prompt suggestions, chat titles,
`recall`'s search phrasing if it ever needs one. Unset means the chat's model, as today. Agent types may say
`model: small`. #63 Q2 stays decided: compaction uses the chat's own model (the cached prefix makes it cheaper).

### 16.3 Microcode (decision A3)

A core is **(provider, model, profile)**. Profiles are public #66 (per-family prompts, `apply_patch` for GPT), which
is still awaiting review (bd marv-cgd.3). This section doesn't approve #66. It only fixes that a profile is chosen per
core, never per step, and that a subagent on another family gets that family's profile.

### 16.4 Cores on other providers (decision A4)

An agent type may name a core on another configured provider (`model: zai/glm-4.6`, `model: chatgpt/gpt-5`), so a
plan's quota can run the workers while the chat runs elsewhere. **Only the person's own agent types may**
(`~/.marv/agents`). A project's `.marv/agents` may name models on the session's provider only. A project file
choosing *where your code is sent* would be an exfiltration channel. This also needs the project trust from §12 N3. A
cross-provider subagent inherits taint and approvals as any subagent does, and the approval for a worktree subagent
names its core.

### 16.5 What's counted (decision A5)

Per request: tokens in, cached and out; cost (reported, estimated, or none); wall time; and the core. Per turn: the
requests, and whether a person or a wake started it. The kernel keeps a **meter** per chat, per folder (the space),
per grant and per core, by day. The event log gets one `Usage { chat, core, requests, tokens_in, cached, out, cost,
estimated, started_by }` **per turn** (not per request: the log stays small). For plan cores it also records the
provider's quota headers when they send them (`remaining`, `reset_at`).

### 16.6 Budgets, nested (decision A6; §8.5 made concrete)

Budgets in dollars a day (tokens a day for unpriced cores), each optional and set only by the person:
**global ⊇ folder ⊇ chat ⊇ grant**. A request is charged to every level above it and runs only if each has room.
**Defaults:** only the grant's $2 (§10). Global, folder and chat budgets are off until the person sets one, and the
desktop warns at 80% of any that is set. A subagent's spend is its parent chat's, as grants already do.

### 16.7 At a limit (decision A7)

- **A person's turn:** it stops before the next request and asks "acme-api reached $10 today: continue this turn,
  raise the budget, or stop". The answer is the person's, and "continue" covers this turn only.
- **An unattended turn:** pauses and notifies (§10.3, "paused"), exactly like a grant's cap.
- **A plan's quota** (the provider refuses): stops with the provider's reset time and offers the person a core switch.
- **Never a silent move to a cheaper core.** It surprises on quality, and on another provider it moves the person's
  code somewhere new without asking.

### 16.8 Switching a chat's core (decision A8)

`/model` mid-chat stays the person's choice. The kernel then (1) checks the new window: if the context is over the new
budget (§11.2), it pages out first, with a notice; (2) switches profile (#66) at that boundary; (3) logs
`CoreSwitched { chat, from, to }`. It costs one cold prefix, and the notice says so.

### 16.9 Where the person sees it (decision A9)

A **Usage** view in the Mesh panel (today and this week, per chat, folder, core and grant, with budgets inline), the
header meter (already), `marv kernel usage [--week]` (from the event log). The TUI's `/cost` keeps its per-session
view and adds the folder's day.

### 16.10 Guard tests (`tests/guards/tests/allocator.rs`)

- `a_projects_agent_type_cant_name_another_provider`
- `a_request_is_charged_to_every_budget_above_it`
- `a_limit_asks_in_a_person_turn_and_pauses_an_unattended_one`
- `no_limit_moves_a_chat_to_another_core`
- `a_core_switch_pages_out_first_when_the_new_window_is_smaller`
- `budgets_are_set_only_by_the_person`
- `usage_events_carry_no_text`

### 16.11 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| A1 | Who picks | Person per chat; agent types per task (subagents); never per step |
| A2 | Little core | One person-set `smallModel` for suggestions and titles; agent types may use `small` |
| A3 | Microcode | A core is (provider, model, profile); #66 decided separately |
| A4 | Other providers | Allowed for the person's own agent types only; never a project's |
| A5 | Counting | Tokens, cached, cost, wall time, core; one `Usage` event per turn; plan quota headers |
| A6 | Budgets | Global ⊇ folder ⊇ chat ⊇ grant, all optional and person-set; only the grant's $2 on by default; 80% warning |
| A7 | At a limit | Ask in a person turn; pause + notify unattended; plan quota stops with its reset time; never a silent core move |
| A8 | Switching | Person only; page out first if needed; `CoreSwitched` event |
| A9 | Visibility | Mesh panel Usage view, `marv kernel usage`, `/cost` adds the folder's day |

### 16.12 Build order

1. Public: `smallModel` (A2) and cross-provider agent types for the person's own files (A4). Needs `ProviderFactory`
   to make another provider's core.
2. Private: the meters and the `Usage` event (A5), then nested budgets with the ask/pause behavior (A6, A7), built on
   #24's `SpendBook`.
3. Private: `CoreSwitched` and the page-out check (A8), after #27's budget.
4. Private: the Usage view and `marv kernel usage` (A9).
5. Guards.

### 16.13 As built (#32, 2026-10-22)

- **A2/A4 (public):** `marv_session::cores` (`parse_core`: `zai/`, `chatgpt/`, `ollama/`, `openrouter/`; `Cores`,
  `resolve_agent_core`, `CoreMaker`); `smallModel` in the person's config; the TUI, `marv -p` and the desktop make
  another provider's core only with its own key or sign-in. Titles have no model job today (a chat's title is its
  first message), so `smallModel` runs the next-step suggestion only.
- **A5:** `marv_kernel::meter` (`TurnUsage`, `Meters`), `KernelEvent::Usage { usage }` once per turn (a side job
  outside a turn, the suggestion or `/compact`, is its own), a turn's id counted once however often the log is read;
  `marv_session::allocator::MeterBook` (one per process, reads the day's log back so other Marvs' turns count);
  quota headers (`marv_provider::quota`, OpenAI style, IETF, ChatGPT's `x-codex-*`) recorded for zai/chatgpt cores.
- **A6/A7:** `marv_kernel::budget` (`~/.marv/mesh/budgets.json`, `BudgetBy::Person`, an unreadable file or a store
  that can't be made counts as reached); met before **every** request (`AgentOptions::before_request` at the loop's
  request phase: steps, retries, continuations, the overflow resend, the wrap-up; each subagent's, its first too; the
  mid-turn and overflow summaries; `/compact`); the person's question is `must_ask` + `once`, one at a time per turn
  (parallel subagents share it), and a no stops the rest of the turn and hands their message back
  (`SessionEvent::Unsent`); a woken turn pauses (`BudgetReached { unattended: true }`, a desktop notification).
  **A global budget covers all spend:** the TUI, the desktop, `marv -p` (metered too; at a limit it asks no one,
  stops and exits 4; no `--approve` flag answers it) and Herdr workers (charged to the spawning chat through
  `MARV_METER_CHAT`, a desktop chat's id only); marv-web's server meters into its own data folder and asks no visitor.
  The suggestion is skipped at a limit. **Not built:** A7's plan-quota bullet (a plan refusing for its quota still
  stops through #159's error, with its reset time when the provider says it, but no core switch is offered yet). Budgets are in dollars
  (`marv kernel budget`, the desktop set an unlimited token cap): an unpriced core's tokens count but meet no limit
  unless a token cap is written by hand. The grant's own cap stays the grant's (`SpendBook`), checked as before.
- **A8:** `configure` with another core keeps the old core until the next turn, which pages out if the conversation is
  over the new budget (estimated from the conversation after a resume), with the old core when its window holds the
  conversation, else with the new one (each notice says which), then logs `CoreSwitched { chat, from, to, paged_out }`;
  a switch back to the same core before a turn is none.
- **A9:** the Mesh panel's usage section (`usage_view`, `set_budget`), `marv kernel usage [--week]`,
  `marv kernel budget`, `/cost` adds the folder's day (TUI, desktop).
- Model names in `Usage` and `CoreSwitched` keep to model-id characters, at most 120. A project inside a `.marv`
  folder (but not Marv's worktrees) has every file protected. Guards: `tests/guards/tests/allocator.rs` (ten) and
  `cores.rs` (the public one).

## 17. Devices and drivers (M9, #17)

Decided 2026-10-20 (D1–D6 all as recommended; issue #17). Most of M9 was settled on the way: MCP is the open bus and its
tools' properties are fixed by the adapter (§13 C6), and drivers' output is Outside-class (§12 N2). What's already
sound, from the code: the MCP client advertises **no** client capabilities (`client.rs:231`: no sampling, roots or
elicitation), server `instructions` never leave the parser, `tools/list_changed` is ignored (the tool list is fixed
at start), and `/mcp add` is deterministic code with no model in the loop, pinned packages and an exact entry the
person approves (`install/mod.rs`). This section writes the device model down and fills the gaps.

### 17.1 What a device is (decision D1)

A **device** is anything that reaches outside the process through a driver: each MCP server, each door (github,
herdr, host, process), the network (web_fetch, networked commands), and later the browser. A chat's **device set** is
fixed at boot (§18) and shown in the chat card and the desktop chat settings. By default it's every device the person
installed and, for a project's servers, trusted. **New:** the desktop chat settings can switch a device off for one
chat (a cache-safe change only at boot, so it takes effect at the next one, §18.2).

### 17.2 What a driver may return (decision D2)

Text, classed Outside (fenced, tainting), capped at the 30k cut with the whole output saved (`output.rs`). Images go
to the vision quarantine when it exists (`docs/SECURITY.md`, Images), never straight to the model. Resource links are
listed, never fetched on their own. Structured content is rendered to text before the fence.

### 17.3 What a device may ask of Marv (decision D3)

Nothing, as today, and stated as a rule: **no sampling** (a device may not spend the person's model or money), **no
roots** beyond the project path Marv chose to pass, **no elicitation** to the model. If elicitation is ever added, it
goes to the person only, like an approval, never to the model. `list_changed` takes effect at the next boot.

### 17.4 Installing and updating drivers (decision D4)

Installing is a privileged act and stays the person's: `/mcp add` and the desktop gallery, an exact pinned entry,
project servers trusted by content hash (`trust.rs`). **Gap:** `npx` dependencies follow their ranges, and a remote
server is whatever is deployed. The approval says so in fixed words. A trust hash covers the entry and its command
files, not what a remote server becomes. Unchanged, noted in `docs/SECURITY.md`.

### 17.5 Devices under a grant (decision D5)

Unattended turns can't call MCP (§10 S2: MCP isn't `auto_safe`), and that stays the default. **Deferred, not
decided:** a per-tool device grant ("this chat may call `linear.list_issues` while I'm away"). It needs M8's build
first, and a read-only claim from a server is advisory (`readOnlyHint`). Revisit when a real service needs it.

### 17.6 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| D1 | Device model | MCP servers, doors, network, browser; per-chat device set fixed at boot; per-chat off switch |
| D2 | Driver output | Outside class; 30k cut + saved; images to quarantine; links never auto-fetched |
| D3 | Device requests | No sampling, roots or model elicitation (as today, now a rule); `list_changed` at next boot |
| D4 | Installing | Person only, pinned, hash-trusted (as today); approval states the npx-range and remote-server limits |
| D5 | Under a grant | No MCP unattended (as §10); per-tool device grants deferred |
| D6 | Guards | `the_mcp_client_advertises_no_capabilities`, `list_changed_never_changes_a_running_tool_list`, `a_device_switched_off_for_a_chat_isnt_offered`, `no_tool_installs_a_driver` |

---

## 18. Boot and firmware (M10, #18)

Decided 2026-10-20 (B1–B5 all as recommended; issue #18). Boot works, but it's implicit: what's fixed for a process's life and
what a child inherits are spread over `SessionBuilder`, `prompt.rs`, `subagent.rs` and the shells.

### 18.1 The boot sequence (decision B1)

One order for every shell, each step reading only what came before:

1. **Firmware settings:** the person's config, then the project's (an option ROM: it may only set the keys a project
   is allowed, 18.5), then env and flags.
2. **The core:** provider, model, profile (§16), model info (window, prices).
3. **The machine check:** the sandbox probe; doors present (gh logged in, Herdr pane).
4. **Identity:** `AgentId`, mesh name (2a); a resumed chat keeps its own.
5. **Trust:** project MCP servers, project instructions (§12 N3).
6. **Devices:** MCP servers started, tool lists read once (§17).
7. **Instructions, skills, agent types, memory** loaded (as trusted, or fenced, §12).
8. **The system prompt and tool specs** assembled, then **frozen** (18.2).
9. **The first message:** session-start hooks (fenced), a resumed conversation and **its taint**, an undo's pending
   note.
10. **Live lookups**, never frozen: rules, grants, budgets, wires. They're checked at each use.

The kernel logs `Booted { chat, shell, core, prompt_hash, tools_hash, devices, resumed }`, hashes and names only, at
the conversation's first request: the tool list is fixed there (the MCP servers have settled and the deferred-or-not
choice is made at that turn), so steps 6 and 8 complete then, not when the process starts. Two boots with the same
hashes have the same cacheable prefix, which makes byte-stability checkable. Built (#33): the order above is the
design; the shells don't yet run it as one sequence (see 18.7).

### 18.2 What's frozen for a process's life (decision B2)

The system prompt bytes, the tool spec list and order, the core and profile, the device set. Changes to memory,
instructions, skills, agent types, device switches or config made mid-session **take effect at the next boot** (a new
chat, `/clear`, a resume) and say so ("saved: applies to new chats and after /clear"). Only `/model` and `/clear`
rebuild mid-chat, both the person's, both paying one cold prefix. A page-out (§11) doesn't touch the frozen part.

### 18.3 What a child inherits (decision B3)

**Narrower, never wider**, for every kind of child:

| | Subagent | Worktree subagent | Herdr-spawned Marv | A sleeping chat woken |
|---|---|---|---|---|
| Boot | its own, from the type | its own, in the worktree | a new process, the person's config only | a reboot of the same chat |
| Tools, devices | its type's list, within the parent's | the same | its own boot | the chat's own |
| Approvals | no egress ones (`for_subagent`) | the same | none of the parent's | its "always" answers (kept in memory, §5.3) |
| Grant | the parent's, narrower | the same | none | the chat's own |
| Taint | inherited, and flows back | the same | `MARV_TAINTED` if the parent was | restored from the saved session |
| Budget | charged to the parent | the same | its own | the chat's own |
| Memory | none (the main agent's) | none | its own boot | the chat's own |

### 18.4 The setup screen (decision B4)

Onboarding, `/setup` and the desktop settings are the BIOS setup: **the only writers of the person's config**, all
person-driven. No tool writes config (as today), and a guard keeps it so.

### 18.5 The option ROM (decision B5)

A project's `.marv/config.json` may set only an **allow-list** of keys: project-scoped ones such as formatter and
checks, and settings that narrow (yolo off, network off). Everything else is ignored with one startup note. Today some
keys are ignored one by one (the mesh's). An allow-list fails closed for keys added later.

### 18.6 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| B1 | Boot sequence | The 10 steps in 18.1, one order for all shells; `Booted` event with prompt and tools hashes |
| B2 | Frozen set | Prompt, tools, core, profile, devices; other changes apply at next boot, with a notice; only `/model` and `/clear` rebuild |
| B3 | Inheritance | Narrower never wider; the table in 18.3 as guards |
| B4 | Setup | Onboarding, `/setup`, desktop settings are the only config writers |
| B5 | Project config | Allow-list of keys, fail closed |

### 18.7 Build order (M9 and M10 together)

1. Public: the project-config allow-list (B5), the "applies at next boot" notices (B2), the guards (D6, B3, B4).
2. Private: `Booted` events with hashes (B1); device set on the chat card; the per-chat device switch (D1).
3. Public: approval wording for npx ranges and remote servers (D4).

**Built (kernel #33, 2026-10-21).** Public: `marv_session::project_config` (B5: `formatter`, `checks`, `"yolo": false`;
everything else named in one note); `commands::APPLIES_AT_NEXT_START` after `/remember` and `/forget` (B2); guards
`devices.rs` (D6: no capabilities, `list_changed` ignored, no tool installs a driver) and `boot.rs` (B3, B4, B5). D4: `install::proposal::{RUNNER_CHANGES, REMOTE_CHANGES}` in every add approval.
Private: `KernelEvent::Booted` (B1, `marv_session::boot`), `Card::devices`, `SessionOptions::devices_off` and the
desktop's per-chat device switches (D1), guards `boot_kernel.rs`. Follow-ups (public): a conversation's MCP tools are
fixed at its first turn (`/mcp add` and logins apply to the next one; `tool_search`/`mcp_call` reach only the
snapshot; a read-only claim only gets stricter mid-conversation); an untrusted project's config only narrows and its
`.marv/checks` don't run (until the person trusts the project with `/trust`: #220's project trust, wired at each shell in the release merge); Marv desktop reads the
same project config. Not done: `web` (the network) can't be switched off per chat; a sleeping chat's saved card has no
devices until it boots again; the boot order of 18.1 is not one function in the code: each shell still assembles it
(`SessionBuilder`, the TUI's `Bridge::start`, the desktop's host), and only its outcome is checked, by `Booted`.

## 19. The team board: shared memory between a rule's chats (M7b, #22)

Decided 2026-10-20 (T1–T7 all as recommended; issue #22). Deferred from M7 until M2 (who may write, §10) and M8 (how its text
is classed, §12) were decided. Both are now decided. **The problem:** a team of chats under one rule coordinates only
by messages. Who's on what ("I'm on `src/api`", "tests are green on main") is repeated in mail, lost across
compactions, and invisible to a chat that joins later. The OS answer is shared memory. Here it's a small, bounded
board the kernel owns.

### 19.1 What it is (decision T1)

One **board per rule**. Entries are `{ by, kind, text, paths, at }`:
- `by` is stamped by the kernel (the chat's mesh name), never by the model;
- `kind` is one of `claim` ("I'm working on"), `note` or `done`;
- `text` is one line of at most 280 characters;
- `paths` are optional workspace paths, shown in §14's overlap view.

The tools are `board_read`, `board_post { kind, text, paths? }` and `board_clear { mine }`. They're offered to every
chat with mesh tools (a stable tool list, §13) and refused with a fixed sentence where no rule covers the chat.

**Accepted deviation (built, #34 review):** each tool also takes an optional `rule` (a rule's number). `board_read`
without it reads every covering board, each under its rule's label; `board_clear` without it clears the chat's own
entries on all of them; `board_post` without it posts to the only covering rule's board, and under two or more is
refused with the list of rules and their labels.

### 19.2 Lifetime and limits (decision T2)

A board lasts as long as its rule, and revoking the rule deletes it. Each entry expires after 24 hours. A board holds
at most 50 entries, oldest dropped first, and each chat at most 5 live claims. The store is
`~/.marv/mesh/boards/<rule>.json`, with the grants store's rules (0600, atomic, locked, unknown fields refused).

### 19.3 Class and taint (decision T3)

Board text is model-written by other chats, so it's **Outside** (§12): `board_read` returns it fenced
(`<untrusted-data source="board">`, defused), and it taints the reader. That's the same as mesh mail, and a
woken-team chat is tainted already.

### 19.4 Who writes (decision T4)

Any chat the rule covers, in a person's turn or an unattended one. A post is a message to the rule's own space, like
a D2 send, so it doesn't ask. **Except after a `chat_read`** (the read-and-relay rule from #25's review): in a
conversation that read another chat, a post asks, as a mesh send does, so a board can't relay what a read rule didn't
allow. The person can post and clear from the desktop. A model can clear only its own entries. A `board_clear` after a `chat_read`
doesn't ask: it only ever removes the chat's own entries, so it carries nothing.

**Built (#34 review, P2-1):** "like a D2 send" is taken literally. A post runs unasked only where the chat could
`mesh_send` under that rule: a `send` rule, on its sending side unless it's mutual. Under a `read` or `call` rule, or on
the receiving end of a one-way rule, every post asks the person (`once`), so a board never widens a rule's direction.

### 19.5 No interrupts (decision T5)

A post wakes nobody and isn't mail: readers see it at their next `board_read`, and `mesh_wait` doesn't wait on it.
This keeps the board free of wake storms and outside §15's interrupt queue. It's memory, not a signal.

### 19.6 Who sees it, and the log (decisions T6, T7)

- **The person:** a Board tab per rule in the Mesh panel (entries, author, age, Clear). The TUI gets `/board`.
- **Event log:** `BoardPost { rule, by, kind, size, paths_count }` and `BoardCleared { rule, by, count }`, never the
  text or the paths.
- **With §14:** claims are advisory. P2's stale-write refusal stays the guarantee, and a claim's paths appear in the
  overlap view next to the observed writes.

### 19.7 Guard tests (`tests/guards/tests/board.rs`)

- `board_text_is_fenced_and_taints_the_reader`
- `the_author_is_stamped_by_the_kernel`
- `no_rule_no_board`
- `a_post_after_chat_read_asks`
- `a_post_wakes_nobody`
- `revoking_the_rule_deletes_its_board`
- `limits_hold` (50 entries, 5 claims, 280 characters, 24 h)
- `board_events_carry_no_text_or_paths`

### 19.8 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| T1 | Shape | One board per rule; claim/note/done entries, kernel-stamped author; three tools, always offered |
| T2 | Lifetime | The rule's; 24 h per entry; 50 entries, 5 claims per chat, 280 chars |
| T3 | Class | Outside: fenced, taints the reader |
| T4 | Writers | Covered chats without asking, except after a `chat_read`; person posts and clears; models clear only their own |
| T5 | Interrupts | None: posts wake nobody |
| T6 | Visibility | Mesh panel Board tab, TUI `/board` |
| T7 | Audit and §14 | `BoardPost`/`BoardCleared` without text; claims advisory, shown in the overlap view |

### 19.9 Build order

All private: (1) store and tools in marv-mesh/marv-session, (2) the after-read rule (reuses #25's `read_chat` flag),
(3) Mesh panel tab and `/board`, (4) events and guards. Builds after #25 and #30 step 2.

### 19.10 Built (2026-10-22, #34)

- **Store:** `marv_kernel::board` (`BoardStore`), beside the rules' store code. A post re-reads `policies.json` under
  the board's own lock and refuses a gone or expired rule; `PolicyStore::revoke` deletes the board under that lock.
  `by` is `BoardBy::Chat(mesh name)` or `BoardBy::Person` (the desktop).
- **Tools:** `marv_mesh::board` and `marv_session::board`. Which board: the live rules covering the chat at either
  end, in any mode (a read rule's chats share one too), newest first ([`Switchboard::board_rules`]). **Deviation:**
  each tool takes an optional `rule` (§19.1's accepted deviation; under two or more rules a post must name one). `board_clear { mine }` needs `mine: true`; a model never clears others'.
  Main agent only (a subagent's tools are its plug's). `board_post` and `board_clear` are `Write` kind with no
  checkpoint: a post runs unasked (as `own_marvs_only`, D2's shape) unless the conversation read another chat, then
  asks `once` with a warning.
- **Person:** the Mesh panel's Rules/Board tabs (pick a rule; entries with author, kind, age, paths on hover; post as
  "you", clear one or all); the TUI's `/board` (read only).
- **Review fixes:** per-author caps (5 claims, 10 notes; a full board drops the poster's own oldest, never another's),
  `done` closes the author's claim naming its paths (else its oldest), one-line text refuses bidi, Zl/Zp and Cf
  characters, an expired rule's board goes at its next post and on the desktop's rules look (`BoardStore::sweep`),
  one refused board file doesn't hide the others, and `you`/`user`/`the-user`/`person` are reserved mesh names.
- **Not yet:** a claim's paths in §14's overlap view (T7's second half): the Board tab shows them on hover only.

## 20. The visible OS: Activity, the log, permissions (M12)

Decided 2026-10-20 (O1–O10 all as recommended; issue #37). Added after the sprint, from the person's question: *how can a
user tell this is running as a kernel, not just some desktop app?* **Today they can't.** The kernel is plumbing:
the registry, scheduler, rules, grants and event log are real, but they surface as settings (the Mesh panel, the
Background list) and terminal commands (`marv kernel replay`). Nobody sees the Linux kernel either. People believe
in an OS because of what it **lets them see** (a process list, a log, a permissions screen) and **does on its own**
(keeps things running, stops runaways, notifies). This section designs the first part. The second is already built
or decided (§5.3, §10, §14, §15).

**The one tell that matters:** a desktop app owns its windows, while **the kernel spans every Marv on the machine**.
A Marv in a terminal and a chat in the desktop appear in one process list, under the same rules, talking to each
other. Everything below is built to show that.

### 20.1 Activity: the task manager (decision O1)

A top-level **Activity** tab in Marv desktop, next to Projects and Notes in the spaces bar. One row per agent on this
machine, from the registry (`Entry`, 3a): desktop chats, terminal Marvs, subagents nested under their parent, shims.

| Column | From |
|---|---|
| Name, folder, kind (chat / terminal / subagent / shim) | `Entry` |
| State: sleeping, loading, idle, busy, **waiting for you**, paused | scheduler `AgentState` (§5.3) + grant pause (§10) |
| Model (the core) | the chat's core (§16) |
| Context: used / budget | §11.2 |
| Spend today; grant badge | meters (§16.5), grants (§10) |
| Woken by: you, rule N, a timer | the last `Wake` event |
| Last active | `Card.last_active_ms` |

It can be grouped **by folder** or **by team** (a rule's chats). Row actions are the person's signals (§15.3): open,
stop, pause or resume, put to sleep, revoke the grant. A team header has **Stop the team**. The scheduler's limit
shows as "6 of 8 loaded".

### 20.2 One process table across apps (decision O2)

Everything in Activity is **kernel state, never the UI's guess**. For terminal Marvs to be real rows, a Marv's
`marv/info` gains `state`, `core` and `context: { used, budget }`: fixed shapes, from the pid-confirmed door, like the
card. A TUI already tells Herdr working or idle. Only this person's Marvs on this machine are listed, because the
discovery folder is theirs (0700). There's no remote listing in this phase (§0.7.1).

### 20.3 The activity log: the console (decision O3)

A **timeline** under Activity that renders the event log (3b) in **fixed sentences per event kind**:
- "acme-web was woken by acme-api (rule 3, hop 1)"
- "grant on acme-api ran 12 calls while you were away"
- "acme-api paused: $2.00 cap"
- "a message to infra was refused: not wired"

The log holds no message text, so neither does the view. You can filter by chat, team or kind, and clicking an entry
opens the chat. **"While you were away":** when the person comes back after 10 minutes or more, one card sums up the
interval from counts alone, in fixed words: "3 chats ran 41 calls · 1 needs you · $0.62 spent". This is the moment
that shows the OS working without them.

### 20.4 Permissions: who can do what (decision O4)

The Mesh panel's rules, the Background list's grants, and the per-chat devices, timers and budgets move to **one
Permissions view**, with two lenses:
- **by rule or grant:** what each permits, its expiry and Revoke;
- **by chat:** "acme-api may: talk to acme-web (rule 3) · read acme-web (rule 4) · work while you're away
  ($0.40/$2) · use devices: github, linear · wake at 09:00 (timer)".

Making or widening a permission stays where it is (the rule dialog, a y/N in the terminal). This view only shows and
revokes.

### 20.5 Notification history (decision O5)

The notifications from §10.3 (needs you, finished, paused) are kept as a list in Activity for the app's run (in
memory, never on disk), so a missed one isn't lost. Clicking an item opens the chat.

### 20.6 The terminal's view (decision O6)

`marv kernel ps` (one-shot, `--watch` for a `top`-like refresh) prints the Activity table. `marv kernel log` prints
the same sentences as the timeline (`replay` stays the counts). These sit beside `usage`, `grants` and `syscalls`
(§13, §16). The TUI gets no new screen: a terminal user has `marv kernel ps` in another pane.

### 20.7 About this Marv (decision O7)

The desktop's About dialog and `marv kernel about` show:
- the kernel version and the tool ABI hash (§13.5);
- this chat's boot hashes (§18.1);
- the limits in force: K = 8, the wake limits, the hop cap, budgets;
- where each store lives.

A support ticket can then quote it without any conversation text (§0.7.4: log, not replay).

### 20.8 Security (decision O8)

- **For the person only.** Activity, the timeline and Permissions are never offered to a model: no tool reads them,
  and `mesh_list` and the card are unchanged. It's a shell surface (§0.4 layer 5), not a new channel.
- **No message text anywhere in it.** Names, states, counts and fixed sentences only. Card words are the person's own
  typing, so showing them back to the person is fine, but they appear in the chat row, not the log.
- **Signals are person commands** with the same webview trust boundary as grants (#24's `docs/SECURITY.md` note).
- New `marv/info` fields are fixed shapes, refused if malformed, and never reach a model except through what
  `mesh_list` already shows.
- **A Marv's state word is in `mesh_list`, its numbers aren't** (built 2026-10-22, review P3-1). A terminal Marv
  that says its own state (`SessionInit::door_state`) is listed with it (idle, busy, waiting), exactly as a desktop
  chat's scheduler state always was: a fixed word, useful to an agent deciding whom to message. Its `live` numbers
  (core, context, metrics, spend, subagents, processes) are the person's only: a door computes them only when asked
  (`{"live": true}`, Activity's collect), the registry never keeps them, and `mesh_list` prints none (guard
  `activity_never_reaches_a_model`, with a `door_state` door).

### 20.9 Metrics: memory, CPU and power (decision O10)

What the Activity rows and the system bar measure, mapped from a task manager to agents, with where each number
comes from.

**Per agent:**

| Task manager | Agent | Source |
|---|---|---|
| Memory % | **Context used ÷ budget** (§11.2) | Exact after each request: the provider's prompt tokens, the same number `nearly_full` uses. Between requests it's an estimate (characters ÷ 4) until the next exact count |
| Memory pressure | Colour **by budget, not window**: green, amber past 60% (attention fades, §0.3.3), red near a page-out | Derived |
| Paging, swap | Page-outs this session; turns in swap | Counted today; swap with #27 |
| CPU % | **Share of the last 60 s spent in model calls** | **New:** a timer around each streamed request in the agent loop |
| user / system / iowait | **model / tools / waiting for you.** Blocked on an approval is I/O wait: the slow device is the person | The same timers, split three ways |
| Clock speed | Output tokens per second | Stream timing |
| CPU cache hit rate | **Prompt-cache hit %**: cached ÷ prompt tokens | `Totals.cached_tokens`, already kept and not shown |
| Power draw | **$ per hour** over the last 10 minutes | Per-request cost, already summed |
| Battery | Budget left today (grant, chat, folder) | §10, §16.6 |

**System-wide (the bar under the table):**

| Task manager | Agent | Source |
|---|---|---|
| Physical memory | **Loaded sessions out of K: "6 of 8 loaded".** The kernel's real memory limit; beyond it agents sleep (§5.3) | Scheduler |
| CPU capacity | **The provider's rate limit** (requests or tokens a minute) when its headers say so; otherwise **throttling events** (429s, retries) as "running hot" | **New:** read rate-limit headers where sent; count retries (`retry.rs`) |
| Per-core graphs | **Per model:** agents on it, tokens/s, $/h, cache hit %, quota left (a plan core's quota shows like a battery, §16.5) | Meters (#32) |
| Disk | Sessions, swap, saved tool outputs | File sizes |
| Network | Egress calls: web fetches, networked commands, MCP calls | Event log counts |

**Real processes, nested under their agent.** An agent also owns real OS processes with real CPU and RAM: dev
servers from the process door (PIDs already tracked, `processes/mod.rs`), sandboxed bash commands while they run,
and local MCP servers (spawned by Marv; their PIDs are to be kept; a remote server has no local process). `/proc/<pid>`
gives true CPU % and memory. They're indented under the agent, as Chrome's task manager lists a tab's renderer and
extensions:

```
AGENT            STATE      CTX     CPU  model/tools/you   $/h    CACHE  MODEL
acme-api         busy       62% ▲   71%   58 / 13 / 0     $1.10   84%   glm-5.3
  ├ dev server   running   180 MB    3%   (real process)
  └ mcp: linear  running    44 MB    0%
acme-web         waiting    38%     12%    4 /  8 / 88    $0.20   91%   glm-5.3
infra            sleeping    —       —                        —      —    sonnet-5.5
─────────────────────────────────────────────────────────────────────────────
Loaded 2 of 8 · glm-5.3: 2 agents, 48 tok/s, RPM 31% of limit · today $3.80 of $10
```

**Caveats, shown as tooltips:**
- Context % moves per step, not continuously.
- "CPU %" isn't scarce the way a real core is: two agents on one model don't take cycles from each other. Only the
  rate limit is shared, so that's the true system capacity.
- Rate-limit headers differ by provider. Where none are sent, the bar shows throttling events, not a percentage.
- Sandboxed commands are short-lived: their CPU shows while they run and folds into "tools" after.

**Where it's computed:** the timers and meters live in the session and kernel (a rolling 60 s window per agent).
Terminal Marvs report the same numbers in `marv/info` (O2), so one table covers both apps. `/proc` reads happen in the
host process only, for PIDs Marv started itself, never a PID a peer or model names.

### 20.10 Decisions (made 2026-10-20, all as recommended)

| # | Question | Recommendation |
|---|---|---|
| O1 | Activity | A top-level tab: every agent on the machine, state, model, context, spend, woken-by; grouped by folder or team; person signals per row and per team |
| O2 | One table | Kernel state only; terminal Marvs report state, core and context in `marv/info` |
| O3 | Log | Timeline of fixed sentences from the event log; filters; a "while you were away" card from counts |
| O4 | Permissions | One view, by rule or grant and by chat; show and revoke only |
| O5 | Notifications | History list for the app's run, in memory |
| O6 | Terminal | `marv kernel ps [--watch]`, `marv kernel log` |
| O7 | About | Kernel version, ABI hash, boot hashes, limits, stores |
| O8 | Security | Person-only, no text, never to a model; signals behind the webview boundary |
| O10 | Metrics | Context ÷ budget as memory; model share of the last minute as CPU, split model/tools/waiting for you; tokens/s, cache hit %, $/h, budget left; system bar: loaded of K, rate-limit headroom or throttling, per-model graphs; real child processes from `/proc`, nested |
| O9 | Public or private | Private kernel for now. Revisit at open core: Activity is the most persuasive thing to show publicly |

### 20.11 Build order

1. Loop timers (model / tools / waiting) and the rolling 60 s window; cache hit % and $/h from the meters; rate-limit
   headers and throttle counts; MCP child PIDs kept (O10). Then the `marv/info` state, core, context and metrics fields
   (O2) and `marv kernel ps` (O6). These make the table real first.
2. The Activity tab, with rows and person signals (O1). Needs #31's signals for pause, resume and stop-the-team; open
   and stop work today.
3. The timeline and "while you were away" (O3), notification history (O5), `marv kernel log`.
4. The Permissions view (O4), after #24 and #25 merge. About (O7).
5. Guards: `activity_never_reaches_a_model`, `the_log_view_has_no_message_text`,
   `a_terminal_marv_appears_with_its_state`, `signals_come_only_from_the_person`.

**Built (2026-10-22, #38, private):** steps 1–5 above. `marv_kernel::{metrics, procstats, activity, console,
permissions, about}`; `marv_session::live` (the session's meter on its door, `collect`, the timeline); `marv/info`'s
`live`; `marv kernel ps [--watch]`, `log`, `about`; the desktop's Activity tab (agents, timeline, notifications,
permissions, about) and the "while you were away" card; guards in `tests/guards/tests/visible_os.rs` (with
`proc_reads_only_marvs_own_pids`). Host-agnostic: the desktop only renders, so `marv kernel serve` (#39) can host the
same data. Not yet: per-chat devices in Permissions, the tool ABI hash and boot hashes in About (§13.5, §18.1 aren't
built), a terminal Marv's own signals (only the desktop's chats take the person's signals: decided 2026-10-22, fine for now; a terminal user has Esc and `/` commands in their own pane), and swap and disk numbers. A desktop chat's pause is on its door (`Switchboard::set_paused`), so `marv kernel ps` shows it.

**The demo it enables (for the video):** three chats under one rule; give the lead a task and close the lid. The lead
wakes two workers, one stops at an approval, a notification arrives ("acme-web needs you"), and Activity shows all
three, their states and spend, with the timeline of who woke whom. Multitasking, IPC, scheduling, permissions and
logging in one shot.

---

## Appendix: surprises found while reading

1. The 2-hop rule exists only for the radio (`radio.rs:104`); inbox wakes have no hop and no overall cap, only
   3/min per wire (`mesh.rs:278`). Auto-wiring without Phase 2's hop commit would let two chats ping-pong forever
   at 3 wakes a minute.
2. Addressed radio messages do wake (`crates/marv-session/src/radio.rs:272-300`); only broadcasts never do.
3. Wires are never persisted (`switchboard.rs:155`, `mesh.rs:117`): there is nothing on disk to migrate.
4. Tainted sessions ask for every `mesh_send`, even on an approved wire and in yolo (`registry.rs:193`): wiring alone
   can't give live comms.
5. Desktop chats share one process but talk through the cross-process protocol, with two prompts per wire from the
   same person, the second shown in the receiving (maybe background) chat (`host.rs:91-113`, `discovery.rs:545`).
6. A chat's mesh name is random per start (`discovery.rs:84-104`), not its UUID.
7. The desktop cap is soft (`lib.rs:437-467`), and the desktop always wakes on mail with no opt-in (`host.rs:313`).
8. The folder-containment conflict rule isn't enforced anywhere.
9. A chat has no space id, and a space's root moves when the person navigates (`lib.rs:273-284`).
