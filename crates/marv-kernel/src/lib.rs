//! marv-kernel: the runtime kernel (docs/architecture/runtime-kernel.md). New in marv-rs, with no Marv (TS) original.
//!
//! Pure data and decisions, below `marv-mesh`: no sockets, no Tokio, time passed in. It grows by phase:
//! - [`identity`] (phase 2a): an agent's id, the folder subtree it works in, and its stable mesh name; who an agent is
//!   as a rule sees it ([`AgentRef`], 2b).
//! - [`mode`] and [`policy`] (phase 2b): the person's rules and the pure [`match_rule`]. Stored and matched only: the
//!   mesh doesn't honor them yet.
//! - [`capability`] (phase 2b): what a rule allows, as an opaque in-process record, re-validated by generation and
//!   only ever delegated narrower. Nothing issues one outside tests yet.
//! - [`store`] (phase 2b): `~/.marv/mesh/policies.json`, read and written through `marv-fs` (the one module here that
//!   does I/O). Nothing in the mesh consults it yet.
//! - [`scheduler`] (phase 1): which agents' sessions are loaded, under a cap K for what it starts itself, with a run
//!   queue for wakes and least-recently-active eviction.
//! - [`registry`] (phase 3): who exists: the agents a Marv knows (its own, and other Marvs seen through their doors),
//!   indexed by folder subtree, by what they can be reached at, and by space root. Seeing grants nothing.
//! - [`grant`] and [`grant_store`] (services): the person's per-chat permission to keep working while they're away
//!   (yolo's set, nothing past it, under an expiry and a daily spend cap), and `~/.marv/mesh/grants.json`.
//! - [`meter`] and [`budget`] (the allocator, §16): what each chat, folder, grant and core used by day, from one
//!   `Usage` event per turn; and the person's nested budgets (global ⊇ folder ⊇ chat ⊇ grant), `budgets.json`.
//! - [`eventlog`] (phase 3): what happened, as [`KernelEvent`]s (never a message's words), and [`replay`], which
//!   rebuilds the registry, the rules and the states from them.
//! - [`writes`] (memory protection, §14.4): each chat's observed write set (the paths its edit tools wrote in the
//!   last 30 minutes) and the overlaps between them. Shown and logged, never scheduled.
//! - [`board`] (the team board, §19): one small board per rule, `~/.marv/mesh/boards/<rule>.json`, its entries
//!   stamped with their author, limited and expiring; deleted with its rule.
//! - [`interrupt`] and [`timer`] (§15): every event that starts, stops or changes a turn as an interrupt with one
//!   priority order and one preemption rule; the person's signals; timers (`~/.marv/kernel/timers.json`).
//! - [`metrics`] and [`procstats`] (§20.9): what the Activity table measures per agent (loop timers, cache hit %,
//!   $/h, throttles), and CPU and memory of the real processes Marv started (only those: one portable layer).
//! - [`activity`] (§20): the visible OS's data model: every agent on the machine as a row, the system bar, and the
//!   table `marv kernel ps` prints. For the person only.
//! - [`console`] (§20.3, §20.5): the log as fixed sentences (no message text), "while you were away" from counts,
//!   and the notification history (in memory only).
//! - [`permissions`] (§20.4): who can do what, by chat, each line with what revokes it; and [`about`] (§20.7).

pub mod about;
pub mod activity;
pub mod board;
pub mod budget;
pub mod capability;
pub mod console;
pub mod eventlog;
pub mod grant;
pub mod grant_store;
pub mod identity;
pub mod interrupt;
pub mod meter;
pub mod metrics;
pub mod mode;
pub mod permissions;
pub mod policy;
pub mod procstats;
pub mod registry;
pub mod scheduler;
pub mod store;
pub mod timer;
pub mod writes;

pub use board::{
    BOARD_VERSION, BoardBy, BoardEntry, BoardError, BoardKind, BoardStore, Clear, ENTRY_TTL_MS, MAX_CLAIMS, MAX_ENTRIES,
    MAX_NOTES, MAX_PATHS, MAX_TEXT_CHARS, NewEntry, Posted, board_path, boards_dir,
};
pub use budget::{
    BUDGET_VERSION, Budget, BudgetBy, BudgetCheck, BudgetScope, BudgetSnapshot, BudgetStore, FileBudgets, Reached, Spent,
    WARN_PERCENT, budgets_path,
};
pub use capability::{Capability, Narrowing, RULE_HOP_LIMIT, Via};
pub use eventlog::{
    CompactedBy, Delivery, EVENT_VERSION, EventLog, Expiries, GRANT_USED_MAX, GrantPauseReason, KernelEvent, KernelState,
    LoggedEvent, LoggedGrant, LoggedRule, NoEvents, PolicyChange, ReadOutcome, ReadRefusal, ReadWhat, Refusal, Totals,
    WakeSkipReason, WireRefusal, parse_log, replay,
};
pub use grant::{
    DAY_MS, DEFAULT_DAILY_TOKENS, DEFAULT_DAILY_USD_MICROS, Grant, GrantBy, GrantDenied, GrantId, GrantSnapshot, Spend, SpendCap,
    day_of, grant_for, worktree_by_default,
};
pub use grant_store::{FileGrants, GRANT_VERSION, GrantChange, GrantStore, NewGrant, grants_path, spend_path};
pub use identity::{AgentId, AgentKind, AgentRef, FolderScope, ScopeError, mesh_name_for, name_base};
pub use interrupt::{
    Interrupt, InterruptAction, InterruptQueue, Kind as InterruptKind, SignalBy, SignalKind, Source as InterruptSource, TurnNow,
    handling, preempts,
};
pub use meter::{CoreShare, MeterScope, Metered, Meters, PlanQuota, StartedBy, TurnUsage};
pub use mode::{Mode, ModeError};
pub use policy::{Access, Denied, Direction, Matched, Policy, PolicyId, PolicySnapshot, RuleBy, RuleError, Subject, match_rule};
pub use registry::{AgentKey, Card, Entry, MAX_CARD_TOKENS, Reach, Registry};
pub use scheduler::{Action, Admit, AgentState, Request, Scheduler};
pub use store::{FileRules, NewRule, POLICY_VERSION, PolicyStore, Rules, StoreError, policies_path};
pub use timer::{
    MAX_AHEAD_MS, PersonTimer, Repeat, SELF_TIMERS_PER_DAY, TURN_DEADLINE_MS, Timer, TimerBy, TimerId, TimerSnapshot, TimerStore,
    TimerVoidReason, WakeMeRefused, may_fire, timers_path, turn_deadline,
};
pub use writes::{MAX_WRITES, Overlap, RecentWrite, WRITE_WINDOW_MS, WriteSet, shared_paths};
