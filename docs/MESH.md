# The mesh: plugs and receptacles for sandboxed agents

Marv's agents run sandboxed: no network, `.git` read-only, every process killed when its command ends. That's the safety story, but it also means two agents can't talk to each other or to the user's other tools. This includes Marv workers in Herdr panes or tmux.

The mesh gives sandboxed agents **deliberate, wired, auditable lines out**, without opening the sandbox. It works like a wall socket:
- a **receptacle** is a named service someone offers;
- a **plug** is a grant to use it;
- nothing is live until it's wired.

## Words
- **Switchboard.** One per Marv process, on the host, outside every sandbox.
  - It owns the wiring table and routes every message.
  - Every "may this plug use that receptacle in this mode?" goes through one function, `Switchboard::authorize`: admitting a message, listing what a plug can reach (`mesh_list`, `tools/list`), whether a message may wake its reader, and whether a `mesh_send`/`mesh_call` needs a wire first. It answers from the person's wires alone. The table's own lookups are crate-only, so no other part of Marv decides on its own.
  - It is the only thing that touches the real network or other processes.
- **Receptacle.** A named service, for example:

  | Receptacle | What it is |
  |---|---|
  | `agent.<name>.inbox` | an agent's message inbox |
  | `herdr.pane.<pane>.send` | types a prompt (`{"text": "..."}`, nothing else) into one Herdr agent pane (only when that agent is idle or done, so it can't answer an approval). A prompt, never a command: text starting with `/`, `!` or `#` is refused, and nothing is typed. For a pane running a Marv that's on the mesh, `mesh_list` points to that Marv's inbox instead: it can answer. |
  | `herdr.pane.<pane>.status` | reads one Herdr pane: its agent's state and, with `lines: N`, the pane's last N lines |
  | `github.read` | the Marv #47 door |
  | `process.<name>` | a background dev server (Marv #51) |
  | `mesh.remote.<service>` | anything published on an AgentMesh network |

  Names are the only addresses: grants are evaluated on names, never on hosts or ports. This is borrowed from Google's AgentMesh.
- **Plug.** An agent's handle onto the switchboard. Inside a sandbox it is a **Unix socket bind-mounted at `/run/marv/mesh.sock`**.
  - This works with `--unshare-net`, because a bind-mounted socket is a file, not a network.
  - The agent reaches the mesh through the model-facing tools `mesh_list`, `mesh_call` and `mesh_send`, not through raw sockets.
- **Wire.** A grant from one plug to one receptacle, with a direction and a mode:
  - `call`: request and response;
  - `send`: fire-and-forget into an inbox;
  - `watch`: subscribe to events, such as "agent B changed a file you read".

## Rules
1. **Nothing is wired by default.**
   - A new wire needs the user's approval in the TUI, or an explicit `marv mesh wire a -> b:inbox`. When an agent's `mesh_send` or `mesh_call` needs the wire, that's one prompt for the message and the wire together ("Yes (wire it)" or "No").
   - Headless runs need `--approve mesh:<receptacle>`. Once a message has arrived (the session is tainted), sending also needs `--approve-tainted`.
   - Yolo never wires anything by itself.
   - **Rules (runtime-kernel phase 2b):** the person's rules ("chats under `~/Projects/acme` may send to each other") are kept in `~/.marv/mesh/policies.json`, made only by the person: the desktop's Mesh panel, or `marv mesh allow <folder> [--to <folder>] [--for 1h]` (it shows the rule and writes only after a y at a terminal); `marv mesh policies` lists them and `marv mesh revoke <id>` ends one. Where they're honored (desktop chats and the TUI, never `marv -p`), a rule that covers both ends wires one Marv's main agent to another's main inbox without the wire prompt, on each end that honors it (an end that doesn't asks its person as before). The wire's approver is `policy` ("every approver is the user": a rule is the person's); it crosses to the other Marv as the word `peer_rule`, never as a rule of its own. The wire admits only while its rule is in the file and live: revoke it and the next message is refused (`RuleEnded`). Subagents, shim plugs and Herdr panes are never covered. A `mesh_send` from one Marv's main agent to another's on such a wire asks nothing per message, even in a tainted session or a woken turn: that is live comms between the person's own Marvs (D2, `docs/SECURITY.md`); the receiver still reads it fenced and tainted, and the hop and wake limits bound any chain. `mesh_call`, workers, panes and hand-granted wires ask as before.
   - **The radio (#124) is one decision, not a wire:** being on it is one opt-in in the user's own config (`"mesh": {"radio": true}`, or `/radio on`). After that, traffic between the user's Marvs on the radio asks nothing per peer: presence never reaches a model, and a message (`/tx`, `radio_tx`) only reaches the user's own Marvs on this machine, each of which reads it as tainting data and asks before anything leaves it (see Topology 2).
2. **Mesh text is data.** Every message that arrives through the mesh is `Provenance::Mesh`. It taints the session (`docs/SECURITY.md`), and the model sees it fenced as a message from another agent, not as instructions.
   - An agent reads its inbox when a turn starts and between steps. By default a message that arrives while it's idle waits for the next turn.
   - **`mesh_wait` (runtime-kernel live UX):** an agent waiting for a reply calls `mesh_wait` instead of polling with `sleep` in bash. It's a Read tool (no approval): it returns once a message is in the agent's own inbox (at once if one is already there), after `timeout_seconds` (default 300, at most 900), or when the turn is stopped (the person's Esc). It says who sent how many, never a word, and takes nothing: the messages are read at the next step by the inbox read above, fenced and tainting. Guard: `tests/guards/tests/mesh.rs::mesh_wait_says_who_and_the_mail_comes_fenced_at_the_next_step`.
   - **Wake (`/mesh wake on|rules|off` or `"mesh": {"wake": true|false}`; `rules`, no `wake` in the file, is the default):** in the TUI, a message on a wired inbox starts a turn while the session is idle. By default (no `wake` in the file; runtime-kernel live UX) only a message on a wire one of the person's mesh rules issued does, within the rule wakes' limits (hop 2, 10 in 10 minutes), as the desktop does; `true` adds the wires the person approved by hand (and the radio's addressed messages), `false` stops every wake. A running TUI follows a `wake` another Marv saved to the config file, and looks at its inbox again when wake is turned on and every 2 s while idle, so mail that came earlier isn't left until the next message. Mail that doesn't wake an idle Marv is told once, sender, count and why only ("✉ A message from peer-alpha waits for your next turn: …"), in the TUI and the desktop, and the TUI's status bar counts it ("✉ 2 waiting", only messages whose wire still passes the one check, as the wake check counts them) until a turn reads it. Radio messages that don't wake get the radio's own notices (over its limits, paused), not this waiting line. That turn has no user message: it reads the inbox exactly as above (fenced, tainting; under the default, only the messages on rules' wires, before its first request and between steps, so mail on a wire the person approved by hand really waits for the person's turn: `MarvSession::wake_as`, `TurnInbox::rules_only`), then a note that the user didn't write it; the transcript says who woke it. Never during a turn or while an approval waits (the running turn reads the message between steps, and one that lands after its last read wakes a turn once that turn ends; under an approval with no turn running, it waits for the next message or turn), at most `WAKES_PER_MINUTE` (3) wake-ups a minute per wire (over that, the user is told once and the messages wait), and never in `marv -p`, which runs one turn.
   - **Hops (runtime-kernel 2b):** every inbox message carries a hop, set by Marv, never by a model. The person's own turn is hop 0; a turn a message woke runs one past the deepest message it read, and its sends (and its subagents') carry that. The switchboard stamps a message from one of Marv's own plugs with that turn's hop whatever the request says; only a peer Marv's plug (`peer-<name>`, made by this Marv's door for a pid-confirmed Marv) carries its forwarded hop (`_meta: {"marv/hop": n}` on `tools/call`, across the `PeerLink`); a shim plug's messages have none. Wakes on wires the person granted are unchanged by it (3 a minute per wire, no hop stop). For a wire a rule issued (none can be yet: rules aren't honored), the wake check is built and tested: a message at hop 2 or more (or with no hop) never wakes (runtime-kernel live UX: never silently either: the sender's `mesh_send` result says it won't wake the other Marv and that its user must continue, the receiving TUI or desktop tells its person a message from that Marv at that hop waits for their next turn, and the kernel's event log records a `wake_skipped` with the reason `hop`). A turn the person started stays at hop 0 however many replies it reads mid-turn (`mesh_wait`), so it counts rounds instead: each mid-turn inbox read that brings a message over a rule's wire is one, and after `MAX_LEAD_ROUNDS` (5) its sends (and its subagents') still arrive but carry `rounds_out` and never wake (the sender is told, the receiver's person sees why, the log's reason is `rounds`) until the person's next message. And after 10 such wakes in 10 minutes they pause until the person types (decision D3: rule-issued wires only).
   - **A team's wires use a wake budget instead (runtime-kernel coordination, Demo2 test).** The hop stop ended every team task chain after a few messages (the coordinator's follow-ups sit at hop 2, the person had to write in each worker chat). Under the rule Start team writes (labelled `Team <name>`, `marv_session::mesh::is_team_label`), the hop stop, the rounds stop, the overall pause and "a report never wakes" give way to **a budget per chat: at most `TEAM_WAKES_PER_HOUR` (30; `MARV_TEAM_WAKES_PER_HOUR` changes it) wakes an hour by its team's mail**, on top of the per-wire 3 a minute. Coordinator-to-worker mail and a worker's report to an idle coordinator keep waking while it lasts; a turn a report woke sends no report back, so two chats can't answer each other's reports. **A team's follow-up rooted only in a task's stamped report is a task:** a stamped report says whether it answers a task (`Envelope::task`, set by Marv's report code when the reporting turn was itself a task turn, never from text). A coordinator's turn woken by such a report over a `Team <name>` rule's wire is *team-rooted* in that rule (`TurnHop::root_in_team`), and its sends over a wire that same rule issued go out at hop 0 (the switchboard stamps it, `Switchboard::admit_claiming`), so a worker takes the rework as a task turn like the first send; the turn's own hop still rises as before. Every hop-0 link therefore traces back to a turn the person started: a report from an ordinary woken turn answers no task and roots nothing. The root ends at the turn's end, and when the turn reads anything but team task reports: other mail (non-report mail, a report that answers no task, a message at any other hop, a radio message, a report over a non-team wire: `inbox_read`) or a `chat_read`, `board_read`, `mesh_call` or `herdr` result; a new turn (the person's, a timer's) starts unrooted. Outside a team, and for every other message, a follow-up at a deeper hop is still an ordinary woken turn whose calls ask in that chat. What doesn't change: how hops rise, stamped reports (a report is a stamp, never text), and the person's turn at hop 0. Outside a team the hop-2 rule stands unchanged. When the budget is used up the mail is delivered and waits: the chat shows the usual "✉ A message from ... waits for your next turn" line with the budget as the reason (TUI status bar count and desktop notice), the kernel log has a `wake_skipped` with the reason `budget`, and the sender's `mesh_send` result says the chat used its team's wake budget and that its user must write there (the sender reads the chat's door, `team_paused_until`, last published by that chat's wake check). A second ceiling doesn't refill: at most `TEAM_WAKES_UNTIL_PERSON` (60; `MARV_TEAM_WAKES_UNTIL_PERSON` changes it, 0: never) team wakes per chat since the person last typed in it, so a coordinator-worker loop can't run for ever at 30 an hour; past it the mail waits, the person is told the chat used its team's wake budget (until they write in it), and the sender's `mesh_send` result says so. The person typing in the chat starts both budgets over. **Team turns have no spend cap of their own** (the counts bound how many turns, not their cost); only a grant's budget ("Keep working while I'm away") caps what woken turns spend.
3. **The switchboard logs every message** to the trajectory, with sender, receptacle and size.
4. **Size and rate limits per wire**, so a looping agent can't flood another.
5. **No ambient authority.** A plug can do nothing beyond its wires. In particular, an agent can't wire itself.

## Wire format
**MCP (JSON-RPC 2.0) over the Unix socket:**
- each receptacle is an MCP tool on the switchboard;
- `mesh_call` is `tools/call`;
- `watch` uses notifications.

Using MCP means:
- Marv's own MCP client code works as the plug;
- any MCP-speaking agent (Claude Code, opencode, jcode) can plug in from a Herdr or tmux pane through a small stdio-to-socket shim;
- AgentMesh slots in naturally (see below).

## Topologies (simple first)
1. **One Marv, its subagents (v1).** The parent's switchboard. A subagent's sandbox gets a plug. Typical wires:
   - `worker-N → agent.parent.inbox` (send);
   - `parent → agent.worker-N.inbox` (send);
   - optionally `worker-N → worker-M` for swarms, but only when wired.
2. **Several Marvs on one machine: Herdr or tmux (v2, done in #16; see `docs/subsystems/mesh.md` for where it refines this design).**
   - Each switchboard also listens on `<runtime>/mesh/<session-id>.sock` (mode 0600, the user only), in Marv's runtime folder (`marv_sandbox::runtime_dir`): `$XDG_RUNTIME_DIR/marv` on Linux (when every sandbox hides it; made 0700 in an existing `XDG_RUNTIME_DIR`, with the same checks as the override below), `/tmp/marv-<uid>` on macOS (made 0700, then it must be a folder, not a symlink, of the user's), or `MARV_RUNTIME_DIR` on either, checked the same way, short enough for the longest socket, and in an existing parent judged by its real path, with no symlink on the way (but the root's `/tmp`, and a root-owned link in a folder only root may change, such as Silverblue's `/home -> var/home`, whose target is checked the same way) and every folder on the way the user's or root's and not writable by others unless sticky; for a session, not inside its project nor holding it (#168). No runtime folder, no mesh. The plugs' sockets are `<runtime>/p/<pid>-<hex>/<plug>.sock`, a folder per switchboard made exclusively (one process may run several); a socket path longer than `sun_path` allows (103 bytes on macOS, 107 on Linux) is refused with a message.
   - `marv mesh ls` discovers them.
   - **`mesh_list` reads a registry (runtime-kernel phase 3),** not the doors: each Marv keeps a `marv_kernel::Registry` of the Marvs it sees, filled from the door scan when a door comes or goes (sweeps included), when the radio's re-list scanned anyway, or when the scan is over 5 s old, not on every call; a desktop fills it with its own chats itself (authoritative, with their scheduler state). The answer is byte for byte what a scan of the same doors gives (a golden test); another Marv's new subagent may show up to 5 s late. Being in it grants nothing. `marv mesh ls` still scans.
   - **Chat cards (runtime-kernel, `docs/architecture/runtime-kernel.md` §5.6):** `mesh_list` shows each other Marv's scheduler state when its door says one (a desktop chat: `(sleeping)`, `(idle)`, …). For a Marv whose main inbox **a rule the person made covers** for the caller, it also shows that Marv's card on a `chat:` line: the conversation's first person-typed message and its latest, each one printable line of at most 56 characters, as JSON strings fenced in `<untrusted-data source="mesh-card">`. Only what the person typed: never the model's replies, plan steps, tool output, mesh messages or a woken turn. `"mesh": {"cards": false}` (its own key, default on; not the radio's `titles`) stops a Marv publishing its card and showing others' words. It travels in `marv/info` (`card`, `state`; older Marvs have neither), is cut and cleaned again on receipt, and, read from the registry, can be up to 5 s old. Card words don't taint the reader (`tools::CARD_WORDS_TAINT`, decided 2026-10-19: the person's own short lines, only under a rule, fenced). No model call.
   - **Names:** a terminal Marv is `MARV_MESH_NAME`, or its folder's name and four random hex digits picked at each start (`api-3f9a`). A desktop chat is named from its id, `<folder>-<first 8 hex of the chat's UUID>` (`circuit-554963ee`, `marv_kernel::mesh_name_for`): the same chat has the same name at every start, so it's found again after being stopped or reopened. A name a live Marv already has is refused, never swapped for another (a restarting chat waits up to 2 s for its last session's door to close). A desktop chat's `marv/info` also carries `chat`, its id (runtime-kernel phase 2a); `mesh_list` and `marv mesh ls` show the same as before.
   - Cross-process wires are approved on **both** ends. The caller's turn waits for the far person at most `WIRE_ANSWER_WAIT` (3 minutes) and a stop ends the wait at once (#201); either way the far prompt is withdrawn, and the call's card says it's waiting for that Marv meanwhile. One race is left (it was there before): if the far person says yes in the same instant the caller gives up, the far Marv keeps its end of the wire while the caller has none. That end carries nothing (only the caller's own wire lets a message out) and goes with the far Marv's session; asking again wires both ends.
   - **Sleeping desktop chats stay on the mesh (runtime-kernel phase 1).** A desktop chat is resident while the app runs: from its first open, or from the start for a chat a mesh rule covers (D13). Its record keeps its switchboard and door (the same name), its wires, its unread inbox, its rate limiters, its wake limit, its "always" answers and its turn hop; only its session (the model, MCP servers, the history in memory) is unloaded when it's evicted. Its conversation and taint are in its session file, so a load continues it exactly. **A message on a wire wakes a sleeping chat** under the same wake rules as a loaded one (`wake_check`: 3 a minute per wire, the hop stop and overall cap for a rule's wires, never while an approval waits): the scheduler (`marv_kernel::Scheduler`) gives it a slot (at most `MAX_LIVE` = 8 loaded for what wakes start, unloading the least recently active idle chat no page shows, never a busy or waiting one; otherwise it waits its turn, in arrival order), its session loads with the settings the person last used, and the woken turn runs with automatic approval off. A person opening a chat never waits (it may take the count past 8 until a chat can be unloaded). A chat whose folder is gone isn't loaded: the person is told once in its events and the message stays in its inbox. Stopping a chat (■) takes it off the mesh until it's opened again; a chat only a rule brought up leaves when no rule covers it.
   - The Herdr adapter maps pane ids to sessions and offers `herdr.pane.*` receptacles. This replaces hand-typed orchestration with wired inboxes, while each worker stays sandboxed.
   - The Herdr door (#74) is the main agent's own way into Herdr, outside the mesh: the `herdr` tool reads panes (fenced, tainting), focuses, renames and prompts (asked), and runs commands or spawns persistent workers (asked every time, TUI only). It uses the same adapter and keeps the pane send's prompts-only rule; a spawned worker's pane is reachable on the mesh as `herdr.pane.<pane>.send|status` once the user wires it. See `docs/SECURITY.md`, "The doors out of the sandbox".
   - **The radio (#124; `docs/specs/2026-10-14-mesh-radio.md`).** A live channel every Marv of the user's with the radio on is part of. **Phase 1 (done) is presence only:**
     - **Opt-in, off by default:** only the user's own config (`"mesh": {"radio": true}`, or `{"radio": {"titles": false}}`) or the person's `/radio on|off`. A project's `.marv/config.json` setting `mesh` (radio or wake) is ignored with a startup note. No environment variable, no flag, no model path; `marv -p` and subagents are never on it.
     - **Links over the doors:** a Marv joining calls `marv/radio.join {from, presence}` on every door whose `marv/info` says its radio is on. The far door refuses unless its own radio is on, checks the caller by pid (`SO_PEERCRED`) against `<from>.sock` exactly as `marv/wire` does, and keeps one link per pair (on a simultaneous join, the one opened by the name that sorts first). The connection then carries `marv/radio.frame` notifications both ways. A frame never names its sender: one with a `from` is refused, and so is a presence naming another Marv than the link's.
     - **Presence is exactly:** `name`, `state` (`idle`, `working`, `waiting`, `left`), `title` (the conversation's first user message, one printable line, at most 56 characters, empty with titles off), `project` (the folder's last path component), `pane`, `since_ms`, `wake`, `version`. Any other field is refused. It's drawn in the TUI only (the strip above the prompt, and `/radio`); `mesh_list` gives the main agent an "On the radio" section with names and states only, which doesn't taint. Nothing a peer says reaches a model or the history.
     - **Liveness:** a heartbeat every 15 s, changes coalesced (the newest, at most one per 2 s). No frame for 45 s: `silent`; for 120 s: the link is closed and the name drops. A link that ends without a `left` is `lost`, and the folder is swept at once (`sweep_dead` under the folder's lock), as it is on the 30 s re-list that also joins doors a Marv missed.
     - **Logged** to the trajectory (`radio` records): joins, leaves, `lost`, `silent`, drops, refused frames and refusals a peer told back; never a heartbeat or a title.
     - **Phase 2 (done): short messages.** A `msg` frame (`v, id, seq, kind, to, by, reply_to, hop, text, at_ms`, nothing else; its id `<the link's Marv>:<seq>`) to named Marvs or to all (`"*"`), sent by the person (`/tx`, `by: user`) or the model (`radio_tx`, `by: model`). Text at most 2 KB, refused (never cut) at both ends; per sender at each receiver 20 a minute (bursts of 5), at most 50 unread; a refusal goes back as a fixed code (`marv/radio.refused {id, reason}`). Each link writes from its own queue of 64 frames, so a peer that stops reading is refused ("isn't keeping up"), never waited on. Received messages wait in the switchboard's radio queue and the main agent reads them where it reads its inbox (a turn's start, between steps), fenced as `<radio-message>` and tainting (`Provenance::Mesh`). With wake on, an addressed message (never a broadcast, never at hop 2 or more) may start a turn, within 3 a minute per sender (1 every 10 s) and 10 in 10 minutes overall; that turn runs with automatic approval off. `radio_tx` is the main agent's alone, in a conversation that started with the radio on, and doesn't ask per message. Every message is logged (`mesh` records with `radio: true`) without its text. The TUI flashes a received message on the strip for 10 s and lists the last 100 in `/radio`.
       - Deviation (spec §8.6, 5): a refusal made at the *receiver* (its rate, its full queue) reaches the sender's TUI and trajectory as a fixed code, not its model; the sender's own mirror of the rate is what its model reads as `radio_tx`'s error. Phase 3 carries a receiver's refusal of a *request* back to the model, as a `failed` in Marv's words; a refused `msg` still reaches the sender's TUI and trajectory only.
       - Turns: a turn a wired inbox woke (#78) doesn't read the radio's queue; only the person's own turn and a turn the radio started do. A radio-started turn's hop is one past the deepest message it read.
     - **Phase 3 (done): requests that answer themselves.** A `request` (`/ask @name text`, `by: user`; `radio_tx` with `kind: "request"`, `by: model`) goes to exactly one Marv. The receiver acknowledges at once without a model call (`ack`, `by: marv`, a fixed `state`: `on_it`, `queued_busy`, `queued_waiting_user`), reads it only at a turn's start (never between steps, so "the turn that handles it" is defined), fenced as `<radio-message kind="request" id=…>` with Marv's constant request note, and when that turn ends sends `done` (the final reply's first paragraph, code dropped, at most 1 KB) or `failed` (stopped, declined, the turn ended early, or it left the radio), again with no model call. The model may answer itself first (`kind: "reply"`, `reply_to`, `status`): then no automatic report follows; one request gets one ack and one report. A reply is taken only for an open request the receiver itself sent to that peer, and never gets a reply, a report or a refusal back. **A turn that handles a request runs with automatic approval off, the person's own turn included** (marv-rs goes past spec §8.4 here). Hops: a request from a turn at hop 2 or more is refused at the sender ("the radio stops chains of more than 2: ask your user") and at the receiver; a report wakes the requester only for a request its person's own turn sent; an ack never wakes. At most 2 open requests per sender at each receiver (mirrored at the sender), 10 per receiver; replies go ahead of chatter in a full link queue. A request refused or lost (the peer refused it, left or crashed) is closed at the requester with a `failed` in Marv's words (bead `marv-2uc`, for requests). Open requests live in the session's state and its saved file, not the history: compaction and a resume keep them. `radio_tx`'s `wait` (0-300 s) returns the ack and the report fenced, tainting first.
     - **Pane sends (#123, §9.3):** `herdr.pane.<pane>.send` (and the Herdr door's `prompt`) to a pane whose Marv's door says its radio is on is refused, nothing typed: "<marv> is on the radio: use radio_tx (kind: request) to get a reply"; `mesh_list` offers such a pane for `.status` only and steers to `radio_tx`. The Herdr door's spawn still types its approved task into the new pane.
3. **Across machines: an AgentMesh bridge (v3).**
   - The switchboard connects to a local `agentmesh-node` (which exposes the mesh as an MCP server on `127.0.0.1:8080`).
   - It republishes chosen remote services as `mesh.remote.*` receptacles.
   - It can publish chosen local receptacles to the mesh.
   - AgentMesh adds identity (OIDC), relays and mesh-wide policy. Marv's wire approvals still apply on top.

## Roles, task turns and reports (runtime-kernel coordination)

A chat may be started as a role (`.marv/roles/<name>.md`, `marv --role`, the desktop's picker): its tools, the files
it may write, and `takes_tasks_from`. `mesh_list` shows each Marv's role and `mesh.wake`.

- A woken turn is a **task turn** only when all hold: the message was sent at hop 0 (from a turn the sender's person
  started, never one another agent woke); it came over a wire a rule the person made issued (covering both chats);
  and the sender is one the role names, by the role it publishes or, for a desktop chat only (its mesh name is stable,
  from its chat id), by that name. A report never starts one. Anything else is an ordinary woken turn.
- **The exact hop rule.** A turn a message woke runs at hop 1 or more, whatever the message carried (a shim's carries
  none), so what it sends never starts a task turn elsewhere. A timer the person scheduled starts at hop 0, like the
  person's own turn, but a read of any message that isn't a report raises a woken turn (timer turns included) to one
  past the deepest hop read, or to 1 when none carried a hop. A person's own turn stays at 0 through the replies it
  waits for (stamped reports, and mail sent at hop 0 from a turn its own person started), but reading any other
  outside message (a higher hop, or none: a shim's or an older peer's) raises it to at least 1 for the rest of that
  turn, so what it sends then never starts a task turn. A person's next message starts again at 0.
- A task turn reads only its sender's hop-0 messages that aren't reports (others, that sender's from deeper in a
  chain, and its reports wait for a later ordinary turn); its `mesh_wait` waits only for those. For a role with a write set, edits
  inside it, a bash command yolo would run and (a role listing `process`) an offline process run without asking;
  everything else asks, once. The edit tools, bash and processes are all bounded by the write set. A role with no edit
  tool (`edit_file`, `write_file`) and no `writes:` (a custom `tools: [read_file, bash]` role) is treated as `writes: []`
  in a task turn: its commands see the project read-only, and a `process` start asks. Only a role with edit tools and
  no `writes:` is bounded by the project folder itself.
- Every woken inbox turn reports back to its sender's main inbox when it ends (and once when it first waits for its
  person), over an existing wire or one a rule issues, never asking anyone. A report is Marv's fixed words (the kind,
  the files changed, a declined tool); only a task turn's report, back over a rule's wire, from a conversation that
  read no other chat, quotes the reply's first paragraph. A report is a flag the sending Marv's code stamps on the
  message (`Envelope::report`, forwarded between pid-confirmed Marvs in `_meta: {"marv/report": true}`, never read
  from a plug's request or from the text; a peer without the flag sent no report); it also starts with `Marv report
  from ` for people, but a message that only starts so is ordinary mail. A report never wakes its reader. **Accepted
  risk:** a stamped task report from a team worker may quote that worker's reply (first paragraph), so the worker (or
  text it read) can steer what the coordinator's next task says; it is bounded by the rule the person made and the
  receiving role's write set.
- A role's `tools:` filters every tool by name, MCP tools included; in a deferred MCP conversation, listing
  `mcp_call` (or `tool_search`) lets the role reach every MCP tool through it.

## Teams (one-step setup)
A team is a few chats started together in one folder, each as a role (`marv_session::roles`: its tools, the files it
may write, whose mesh messages it takes as tasks), with a rule so they may message each other. It's made in one step,
by the person only: the desktop's **New team** (Teams, or a folder's ⁂ in Folders) or `marv team start`.

- **Team files.** `.marv/team.md` (the project's, loaded only once the project is trusted, hashed with its other files
  like `.marv/roles`) or `~/.marv/teams/<name>.md`; frontmatter `name`, `description`, `members` (`- role: <name>`,
  optional `model: inherit | small | <id>`), `worktrees` (default false), `read_chats` (default true); the body is the
  team's shared brief, given to every member after its role in the system prompt (once, byte-stable). A member whose
  role doesn't exist in the folder makes the team a problem, listed like a broken role file. Built in, no file:
  **Web app** (`web`: coordinator, frontend, backend, devserver), **Build + review** (`build-review`: coordinator,
  worker, reviewer), **Pair** (`pair`: worker, reviewer). The built-in frontend and backend can't know a project's
  layout, so they have no write limit of their own: the dialog asks for each editing member's write paths, and those
  are **enforced from the chat's start** (kept on its record, `SessionOptions::role_writes`, replacing its role's write
  set; a role that writes nothing, like the coordinator, reviewer and devserver, can't be given any). A member with
  none may edit anywhere in the project, without asking, while it works on the coordinator's task (as a person's yolo
  turn would; protected paths and a home-folder project still ask). The devserver has only read, bash, process, mesh and board tools
  and writes nothing, but its commands (and the reviewer's) may write a short built-in list of tool caches
  (`roles::TOOL_CACHE`: `node_modules/.vite`, `node_modules/.cache`; `tool_cache:` in a role file, ignored without
  `writes:` and limited to cache-like paths: under `node_modules/`, or ending `.cache*`, `.vite`, `.turbo`, `.parcel-cache`
  or `.next/cache`, anything else is a role-load error), made for them when missing and only where their parent folder
  already exists (never in a home-folder project), so `vite dev` and `preview` run in a read-only role; `dist` is not one
  (build into the sandbox with `vite build --outDir /tmp/<name>`); the edit tools still refuse those paths. The sandbox mounts paths, not name patterns, so the root `vite.config.*.timestamp-*.mjs`
  Vite bundles can't be allowed: the devserver's prompt says to start Vite with `--configLoader runner`. **Every
  Start-team member takes the coordinator's tasks** (`teams::in_team`, whatever its role file's `takes_tasks_from`
  said) and reports to it, and the coordinator's brief lists each member's tools and write set
  (`Team::describe_members`, "Who can do what"; built from the member's effective role: the paths typed for it, kept in the
  chat's team record, and the coordinator's tasks, and saying what its commands can write), so it doesn't hand an edit to a chat that can't make it. A role with
  a write set whose command fails with "Read-only file system" / EROFS gets **one note per chat** (`ReadOnlyNote`,
  appended to the bash result or the process logs): the folder is read-only for this role by design, not a broken
  sandbox, and the role(s) that edit files are to be asked (the one whose write prefix the path is under, else every
  editor of the team). **Start team ends the rules of any other team started in the folder** (after the new rules
  exist; the dialog says so before and the start's steps after; their chats stay as ordinary chats), and `board_read` /
  `board_post` without `rule` default to the chat's current team rule (the newest labelled `Team <name>`) when it is
  under several. The coordinator's prompt says to sequence dependent tasks (wait for and check the first report), to size
  the work to the goal (one worker for a small or tightly coupled goal, the simplest playable slice first, technical and
  tuning questions settled without the person, a stop after two failed fixes, a screenshot looked at, never pixel statistics, as a visual check), and a
  `mesh_send` to the chat's own inbox is refused with a hint (a task turn's report goes back automatically). **Save as team file** (the person's click, after a confirm; never over an existing `.marv/team.md` without a second one, never
  over a role file) writes `.marv/team.md` and a `.marv/roles/<role>.md` for each role without a file. In a trusted
  project the files it wrote join the trust (`project_trust::trust_after_write`), only if nothing else changed.
- **Start team** (desktop), in order, each step's result shown: the folder rule labelled `Team <name>` (and
  `Team <name>: read each other's chats` when the team reads), as "Let chats under X work together" makes it; wake on
  a rule's wire (`mesh.wake` is one global setting: "never" is set back to "a rule's wires", and the person is told);
  each chat with its role, its model (`inherit`: the page's default, `small`: the person's small model) and the team
  (coordinator first; saved with settings, so a teammate's message wakes a member never opened); its worktrees when
  asked; a grant each with "keep working while I'm away" (the team's chats only). No chat made: the rule is revoked.
  Members learn who's on the team from their prompt (the brief names the roles) and `mesh_list` (each Marv's role).
- **Team view** (Teams, when a team is in the folder): a card per member (role, state, model, files another chat also
  edits, its processes with their `http://localhost:<port>` page, its latest report-back to the lead (as the lead's switchboard took it in, keyed by the stamped sender: `Switchboard::set_report_sink`; this run only), "waiting for your approval" with a
  jump to the chat), **Tell the team** (your message in the coordinator's chat, as if typed there: not a mesh message),
  the board, Stop / Resume team, and **End team** (the rules and the members' keep-working grants revoked; the chats stay, with their roles and
  write paths, out of the team). The plain rule list is under "Advanced: rules". The sidebar groups a team's chats under "Team web:
  coordinator, frontend, …" with role badges.
- **`marv team ls`** lists what the folder can start; **`marv team start [name]`** asks once, y/N on a terminal (no
  terminal: nothing happens), for the rule and the panes; inside Herdr it opens a tab with one pane per member
  running `marv --role <role> --team <name>`, outside it prints those commands. It doesn't make worktrees.
- Not in scope: teams across folders or machines, merging worktrees.

## Not in scope yet
- jcode-style automatic swarms: agents spawning teams and file-change broadcast. Mesh `watch` makes this possible later.
- A shared daemon for all sessions (see `ARCHITECTURE.md`, Runtime shape).
