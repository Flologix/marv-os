//! The allocator (`docs/architecture/runtime-kernel.md` §16.10): one `Usage` event per turn with numbers and ids only;
//! budgets (global ⊇ folder ⊇ chat ⊇ grant) set only by the person, every request charged to each level above it;
//! at a limit a person's turn asks and an unattended one pauses; never a silent move to another core; a core switch
//! pages out first when the new window is smaller.
//!
//! Real sessions with a scripted model, a real budgets file in a private config folder, the real meters. Every guard
//! says how it was proven: "reverted X → this test failed".

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use marv_guards::{Recorder, Scripted, TempDir, say, tool_call, use_tools};
use marv_kernel::{BudgetScope, BudgetStore, FileBudgets, KernelEvent, SpendCap, StartedBy};
use marv_provider::{Provider, ProviderFactory};
use marv_session::allocator::{Charge, MeterBook, TurnStart};
use marv_session::{MarvSession, ProviderChoice, SendOptions, SessionInit};
use marv_tools::Decision;
use serde_json::json;

/// Kernel events kept in memory.
#[derive(Default)]
struct Events(Mutex<Vec<KernelEvent>>);
impl marv_kernel::EventLog for Events {
    fn append(&self, _at_ms: u64, event: KernelEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// A factory on one scripted provider that records every model it's asked for.
struct Factory(Arc<Scripted>, Arc<Mutex<Vec<Option<String>>>>);
impl ProviderFactory for Factory {
    fn id(&self) -> &str {
        "openrouter"
    }
    fn model(&self) -> &str {
        "big"
    }
    fn make(&self, model: Option<&str>, _info: Option<&marv_provider::ModelInfo>) -> Arc<dyn Provider> {
        self.1.lock().unwrap().push(model.map(String::from));
        self.0.clone()
    }
}

struct Setup {
    _dir: TempDir,
    config: PathBuf,
    home: PathBuf,
    root: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        let dir = TempDir::new(&format!("alloc-{tag}"));
        let (config, home) = (dir.path().join("home/.marv"), dir.path().join("home"));
        let root = home.join("acme");
        std::fs::create_dir_all(&root).unwrap();
        Self { config, home, root, _dir: dir }
    }

    fn store(&self) -> BudgetStore {
        BudgetStore::new(&self.config, &self.home).unwrap()
    }

    fn book(&self, events: Option<Arc<Events>>) -> Arc<MeterBook> {
        Arc::new(MeterBook::new(
            None,
            events.map(|e| e as Arc<dyn marv_kernel::EventLog>),
            Some(Arc::new(FileBudgets::new(self.store()))),
        ))
    }

    /// A turn already run today in this folder by chat `chat` that cost `usd`.
    fn spent(&self, book: &Arc<MeterBook>, chat: &str, usd: f64) {
        let turn = book.begin(TurnStart {
            chat: chat.into(),
            folder: self.root.clone(),
            core: "openrouter/big".into(),
            grant: None,
            started_by: StartedBy::Person,
        });
        turn.charge("openrouter/big", Charge { tokens_in: 10, out: 1, cost_usd: Some(usd), ..Charge::default() });
        turn.finish();
    }
}

fn dollars(usd: u64) -> SpendCap {
    SpendCap { usd_micros: usd * 1_000_000, tokens: u64::MAX }
}

/// One turn in a session metered by `book`, chat `chat`, answering approvals with `approver`.
async fn turn(
    s: &Setup,
    book: &Arc<MeterBook>,
    chat: &str,
    model: Arc<Scripted>,
    approver: Arc<Recorder>,
) -> Vec<Option<String>> {
    turn_with(s, book, chat, model, approver, |_| {}).await.0
}

/// [`turn`] with `edit` on the session's init; also what the session suggested afterwards.
async fn turn_with(
    s: &Setup,
    book: &Arc<MeterBook>,
    chat: &str,
    model: Arc<Scripted>,
    approver: Arc<Recorder>,
    edit: impl FnOnce(&mut SessionInit),
) -> (Vec<Option<String>>, Option<String>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut init = SessionInit::new(&s.root, ProviderChoice::Factory(Arc::new(Factory(model, asked.clone()))));
    (init.diagnostics, init.checks, init.checkpoints) = (false.into(), false, false);
    init.meters = Some(book.clone());
    init.meter_chat = Some(chat.into());
    init.approve = Some(approver);
    edit(&mut init);
    let session = MarvSession::new(init).unwrap();
    let mut events = session.send("fix the parser", SendOptions::default()).unwrap();
    while events.next().await.is_some() {}
    let suggested = session.suggest(tokio_util::sync::CancellationToken::new()).await;
    session.close().await;
    (asked.lock().unwrap().clone(), suggested)
}

/// A reply that calls `calls` and reports what it cost.
fn costing(calls: Vec<marv_core::ToolCall>, usd: f64) -> Vec<marv_core::ProviderEvent> {
    let mut events: Vec<marv_core::ProviderEvent> = calls.into_iter().map(marv_core::ProviderEvent::ToolCall).collect();
    events.push(marv_core::ProviderEvent::Usage(
        marv_core::Usage { prompt_tokens: 100, completion_tokens: 10, cached_tokens: None, cost: Some(usd) },
        marv_core::ReplyEnd::default(),
    ));
    events.push(marv_core::ProviderEvent::Done { reason: Some("tool_calls".into()) });
    events
}

/// A request is charged to every level above it: the chat, its folder and every folder it is in, and everything; a
/// folder's budget at its limit stops a chat whose own budget has room, and so does the global one.
///
/// Proven: dropped the folder budgets from `marv_kernel::budget::covering` → the acme folder's $5 didn't stop the chat.
#[tokio::test]
async fn a_request_is_charged_to_every_budget_above_it() {
    let s = Setup::new("charged");
    let store = s.store();
    let acme = marv_kernel::FolderScope::new(&s.root, &s.home).unwrap();
    store.set(BudgetScope::Chat("c1".into()), Some(dollars(100)), 1).await.unwrap();
    store.set(BudgetScope::Folder(acme.clone()), Some(dollars(5)), 1).await.unwrap();
    store.set(BudgetScope::Global, Some(dollars(50)), 1).await.unwrap();
    let book = s.book(None);
    s.spent(&book, "c2", 3.0);
    let live = book.begin(TurnStart {
        chat: "c1".into(),
        folder: s.root.join("api"),
        core: "openrouter/big".into(),
        grant: None,
        started_by: StartedBy::Person,
    });
    live.charge("openrouter/big", Charge { cost_usd: Some(2.5), ..Charge::default() });
    for scope in [BudgetScope::Global, BudgetScope::Folder(acme.clone())] {
        assert_eq!(book.spent(&scope).usd_micros, 5_500_000, "{scope:?}: both chats, the live turn too");
    }
    assert_eq!(book.spent(&BudgetScope::Chat("c1".into())).usd_micros, 2_500_000);
    let check = book.check("c1", &s.root.join("api"));
    assert_eq!(check.limits.iter().map(|r| r.scope.clone().unwrap()).collect::<Vec<_>>(), [BudgetScope::Folder(acme)]);
    assert!(check.warnings.is_empty());
}

/// At a limit a person's turn asks before its request (a `must_ask`, `once` question: nothing but the person answers
/// it), and "no" sends nothing; an unattended turn pauses without asking anyone, and the person is told.
///
/// Proven: removed the `if self.unattended` pause in `TurnMeter::gate` → the woken turn asked the approver and ran.
#[tokio::test]
async fn a_limit_asks_in_a_person_turn_and_pauses_an_unattended_one() {
    let s = Setup::new("asks");
    s.store().set(BudgetScope::Chat("c1".into()), Some(dollars(1)), 1).await.unwrap();
    let events = Arc::new(Events::default());
    let book = s.book(Some(events.clone()));
    s.spent(&book, "c1", 1.0);

    let model = Scripted::new(vec![say("Fixed.")]);
    let no = Recorder::new(Decision::No);
    turn(&s, &book, "c1", model.clone(), no.clone()).await;
    let asked = no.requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].must_ask && asked[0].once, "only the person answers it");
    assert!(asked[0].preview.title.contains("this chat reached its $1.00 budget for today"), "{}", asked[0].preview.title);
    assert!(model.requests.lock().unwrap().is_empty(), "no: nothing was sent");

    let yes = Recorder::new(Decision::Yes);
    turn(&s, &book, "c1", model.clone(), yes.clone()).await;
    assert_eq!((yes.requests().len(), model.requests.lock().unwrap().len()), (1, 1), "yes: this turn goes on");

    // Unattended: a wake's turn pauses before its request; nobody is asked.
    let wake = book.begin(TurnStart {
        chat: "c1".into(),
        folder: s.root.clone(),
        core: "openrouter/big".into(),
        grant: None,
        started_by: StartedBy::Wake,
    });
    let anyone: Arc<dyn marv_tools::Approver> = Recorder::new(Decision::Yes);
    let told = Arc::new(Mutex::new(Vec::new()));
    let sink = told.clone();
    let notify: marv_session::allocator::OnBudget = Arc::new(move |n| sink.lock().unwrap().push(n));
    assert!(!wake.gate(Some(&anyone), &tokio_util::sync::CancellationToken::new(), &notify).await);
    assert!(matches!(told.lock().unwrap().as_slice(), [marv_session::allocator::BudgetNotice::Paused(_)]));
    assert!(events.0.lock().unwrap().iter().any(|e| matches!(e, KernelEvent::BudgetReached { unattended: true, .. })));
}

/// No limit moves a chat to another core: at its budget's limit a turn stops (or goes on, if the person says so) on
/// the chat's own core; no other model or provider is ever asked for, not even a subagent's small one.
///
/// Proven: made `MarvSession::budget_gate` go on whatever the gate said (the stop removed) → with "no" the request ran
/// (on the chat's core: the guard is that nothing else happens at a limit).
#[tokio::test]
async fn no_limit_moves_a_chat_to_another_core() {
    let s = Setup::new("no-move");
    s.store().set(BudgetScope::Global, Some(dollars(1)), 1).await.unwrap();
    let book = s.book(None);
    s.spent(&book, "other", 2.0);
    let model = Scripted::new(vec![
        use_tools(vec![tool_call("a1", "agent", json!({"description": "look", "prompt": "list the files"}))]),
        say("Looked."),
        say("Done."),
        say("run the tests"),
    ]);
    // A small model on another provider and a maker for its core are there: a limit still never reaches for them.
    let cores = Arc::new(Mutex::new(Vec::new()));
    let with_cores = |cores: Arc<Mutex<Vec<String>>>, model: Arc<Scripted>| {
        move |init: &mut SessionInit| {
            init.small_model = Some("zai/glm-4.5-air".into());
            init.cores = Some(Arc::new(move |kind: marv_provider::ProviderKind, name: &str| {
                cores.lock().unwrap().push(format!("{}/{name}", kind.id()));
                Some(Arc::new(Core("air", Arc::new(Windowed(model.clone(), 100_000)))) as Arc<dyn ProviderFactory>)
            }));
        }
    };
    let (made, suggested) =
        turn_with(&s, &book, "c1", model.clone(), Recorder::new(Decision::No), with_cores(cores.clone(), model.clone())).await;
    assert!(model.requests.lock().unwrap().is_empty(), "stopped, not moved");
    assert!(made.iter().all(Option::is_none), "no other model asked for: {made:?}");
    assert_eq!(suggested, None, "no side job at a limit, not even on the small model");
    assert!(cores.lock().unwrap().is_empty(), "no other core made: {:?}", cores.lock().unwrap());
    let (made, _) =
        turn_with(&s, &book, "c1", model.clone(), Recorder::new(Decision::Yes), with_cores(cores.clone(), model.clone())).await;
    assert!(made.iter().all(Option::is_none), "going on stays on the chat's core: {made:?}");
    assert!(cores.lock().unwrap().is_empty(), "no other core made: {:?}", cores.lock().unwrap());
}

/// The request after the one that crossed a cap asks, mid-turn: a turn's first reply costs $1.50 against a $1 budget
/// and calls a tool; its next request asks first, and "no" sends nothing more.
///
/// Proven: removed the main agent's `before_request` in `turn_body` → the second request went out unasked.
#[tokio::test]
async fn the_request_after_the_one_that_crossed_a_cap_asks() {
    let s = Setup::new("mid-turn");
    s.store().set(BudgetScope::Chat("c1".into()), Some(dollars(1)), 1).await.unwrap();
    std::fs::write(s.root.join("notes.txt"), "remember the milk").unwrap();
    let book = s.book(None);
    let model =
        Scripted::new(vec![costing(vec![tool_call("r1", "read_file", json!({"path": "notes.txt"}))], 1.5), say("Read it.")]);
    let no = Recorder::new(Decision::No);
    turn(&s, &book, "c1", model.clone(), no.clone()).await;
    assert_eq!(model.requests.lock().unwrap().len(), 1, "the second request wasn't sent");
    let asked = no.requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].tool, "budget");
}

/// The same through a subagent: its own requests meet the budget (its first one too), and the one after its reply
/// crossed the cap asks.
///
/// Proven: removed the subagent's `before_request` in `subagent::drive` → its second request went out unasked.
#[tokio::test]
async fn a_subagents_requests_meet_the_budget_too() {
    let s = Setup::new("sub");
    s.store().set(BudgetScope::Chat("c1".into()), Some(dollars(1)), 1).await.unwrap();
    std::fs::write(s.root.join("notes.txt"), "remember the milk").unwrap();
    let book = s.book(None);
    let model = Scripted::new(vec![
        use_tools(vec![tool_call("a1", "agent", json!({"description": "read", "prompt": "read notes.txt"}))]),
        costing(vec![tool_call("r1", "read_file", json!({"path": "notes.txt"}))], 1.5),
        say("The notes say milk."),
        say("Done."),
    ]);
    let no = Recorder::new(Decision::No);
    turn(&s, &book, "c1", model.clone(), no.clone()).await;
    assert_eq!(model.requests.lock().unwrap().len(), 2, "the main agent's and the subagent's first; nothing after");
    assert_eq!(no.requests().len(), 1, "one question");
}

/// An unreadable budgets file counts as every budget reached, in a real session: the person's turn asks before its
/// first request, and a turn a mesh message woke pauses without asking anyone.
///
/// Proven: made `MeterBook::check` treat a refused file as no budgets → both turns ran.
#[tokio::test]
async fn an_unreadable_budgets_file_asks_a_person_and_pauses_a_wake() {
    use marv_mesh::approval::WireRequest;
    use marv_mesh::switchboard::{NoTrajectory, Switchboard};
    use marv_mesh::{ReceptacleName, WireMode};
    let s = Setup::new("corrupt");
    std::fs::create_dir_all(s.config.join("mesh")).unwrap();
    let path = marv_kernel::budgets_path(&s.config);
    std::fs::write(&path, "{ not json").unwrap();
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let book = s.book(None);

    let runtime = TempDir::short("alloc-wake-rt");
    let mesh = Arc::new(Switchboard::new("aw", runtime.path().join("plugs"), Arc::new(NoTrajectory)));
    let cancel = tokio_util::sync::CancellationToken::new();
    tokio::spawn(Arc::clone(&mesh).serve(cancel.clone()));
    let model = Scripted::new(vec![say("Fixed."), say("Woke.")]);
    let person = Recorder::new(Decision::No);
    let mut init = SessionInit::new(&s.root, ProviderChoice::Factory(Arc::new(Factory(model.clone(), Arc::default()))));
    (init.diagnostics, init.checks, init.checkpoints) = (false.into(), false, false);
    init.meters = Some(book);
    init.meter_chat = Some("c1".into());
    init.approve = Some(person.clone());
    init.mesh = Some(mesh.clone());
    let session = MarvSession::new(init).unwrap();
    let drain = |mut turn: marv_session::session::Turn| async move { while turn.next().await.is_some() {} };
    drain(session.send("fix the parser", SendOptions::default()).unwrap()).await;
    assert_eq!(person.requests().len(), 1, "the person was asked");
    assert!(person.requests()[0].preview.title.contains("can't be read"), "{}", person.requests()[0].preview.title);
    assert!(model.requests.lock().unwrap().is_empty());

    let worker = mesh.plug_next("worker").unwrap();
    let inbox = ReceptacleName::new("agent.parent.inbox").unwrap();
    let request = WireRequest { from: worker.id.clone(), to: inbox.clone(), mode: WireMode::Send, asked_by: worker.id.clone() };
    mesh.grant(request, marv_mesh::wire::Approver::Tui).await.unwrap();
    mesh.deliver(&worker.id, inbox.as_str(), WireMode::Send, json!({"text": "fix it"})).await.unwrap();
    let wake = marv_session::mesh::wake_check(&mesh, &mut Default::default(), 0).wake.expect("a wake");
    drain(session.wake(&wake).unwrap().expect("a woken turn")).await;
    assert_eq!(person.requests().len(), 1, "a woken turn asks nobody");
    assert!(model.requests.lock().unwrap().is_empty(), "it paused");
    cancel.cancel();
    session.close().await;
}

/// Budgets are set only by the person: in the user's own config folder (never a project's: a relative folder is
/// refused, and a project's `.marv/mesh/budgets.json` is never read), made by `BudgetBy::Person` only, and no tool
/// the model is offered touches them.
///
/// Proven: removed the absolute-folder check in `BudgetStore::new` → a relative (project) folder was accepted.
#[tokio::test]
async fn budgets_are_set_only_by_the_person() {
    let s = Setup::new("person");
    assert!(BudgetStore::new(Path::new(".marv"), &s.home).is_err(), "a relative folder is the project's");
    // A project's own budgets file changes nothing: the session reads the person's only.
    std::fs::create_dir_all(s.root.join(".marv/mesh")).unwrap();
    std::fs::write(
        s.root.join(".marv/mesh/budgets.json"),
        r#"{"version":1,"generation":1,"budgets":[{"scope":"global","cap":{"usd_micros":0,"tokens":0},"by":"person","set_at_ms":1}]}"#,
    )
    .unwrap();
    let book = s.book(None);
    assert!(book.check("c1", &s.root).limits.is_empty(), "the project's file isn't the person's");
    for by in ["model", "mesh", "project", "env", "flag", "tool"] {
        let budget = json!({"scope": "global", "cap": {"usd_micros": 1, "tokens": 1}, "by": by, "set_at_ms": 1});
        assert!(serde_json::from_value::<marv_kernel::Budget>(budget).is_err(), "{by}");
    }
    let names: Vec<String> = marv_tools::builtin_tools().iter().map(|t| t.name().to_string()).collect();
    assert!(names.iter().all(|n| !n.contains("budget")), "{names:?}");
}

/// A turn's `Usage` event carries numbers and ids only: none of the person's, the model's or a tool's words, and a
/// logged line with any field more is refused.
///
/// Proven: removed `deny_unknown_fields` from `TurnUsage` → a line with a `text` field was read.
#[tokio::test]
async fn usage_events_carry_no_text() {
    let s = Setup::new("no-text");
    let events = Arc::new(Events::default());
    let book = Arc::new(MeterBook::new(None, Some(events.clone() as Arc<dyn marv_kernel::EventLog>), None));
    std::fs::write(s.root.join("secret-notes.txt"), "PURPLE-ELEPHANT-7").unwrap();
    let model = Scripted::new(vec![
        use_tools(vec![tool_call("r1", "read_file", json!({"path": "secret-notes.txt"}))]),
        vec![
            marv_core::ProviderEvent::TextDelta("The notes say ORANGE-WALRUS.".into()),
            marv_core::ProviderEvent::Usage(
                marv_core::Usage { prompt_tokens: 120, completion_tokens: 9, cached_tokens: Some(100), cost: Some(0.01) },
                marv_core::ReplyEnd::default(),
            ),
            marv_core::ProviderEvent::Done { reason: Some("stop".into()) },
        ],
    ]);
    turn(&s, &book, "c1", model, Recorder::new(Decision::Yes)).await;
    let usage: Vec<_> = events.0.lock().unwrap().iter().filter(|e| matches!(e, KernelEvent::Usage { .. })).cloned().collect();
    assert_eq!(usage.len(), 1, "one per turn: {usage:?}");
    let line = marv_kernel::LoggedEvent::new(1, usage[0].clone()).line();
    for words in ["fix the parser", "PURPLE", "ORANGE", "secret-notes", "read_file"] {
        assert!(!line.contains(words), "{words} in {line}");
    }
    let KernelEvent::Usage { usage } = &usage[0] else { unreachable!() };
    assert_eq!((usage.requests, usage.tokens_in, usage.cached, usage.cost_micros), (1, 120, 100, Some(10_000)));
    let mut value: serde_json::Value = serde_json::from_str(&line).unwrap();
    value["usage"]["text"] = json!("a word");
    let (parsed, skipped) = marv_kernel::parse_log(&value.to_string());
    assert!(parsed.is_empty() && skipped == 1, "a field more is refused");
}

/// A scripted model with a window.
struct Windowed(Arc<Scripted>, u64);
impl Provider for Windowed {
    fn name(&self) -> &str {
        "windowed"
    }
    fn context_length(&self) -> Option<u64> {
        Some(self.1)
    }
    fn stream(&self, history: &[marv_core::ChatTurn], options: marv_provider::StreamOptions) -> marv_provider::EventStream {
        self.0.stream(history, options)
    }
}

/// A factory named `model`, on one provider.
struct Core(&'static str, Arc<dyn Provider>);
impl ProviderFactory for Core {
    fn id(&self) -> &str {
        "openrouter"
    }
    fn model(&self) -> &str {
        self.0
    }
    fn make(&self, _model: Option<&str>, _info: Option<&marv_provider::ModelInfo>) -> Arc<dyn Provider> {
        self.1.clone()
    }
}

/// `/model` to a core with a smaller window: the next turn pages out first, summarized by the old core (which can
/// still read the whole conversation), so the new core's first request starts from a summary that fits; the switch is
/// logged as `core_switched` with `paged_out`.
///
/// Proven: removed the switch's page-out in `turn_body` → the summary was asked of the new, smaller core.
#[tokio::test]
async fn a_core_switch_pages_out_first_when_the_new_window_is_smaller() {
    let s = Setup::new("switch");
    let events = Arc::new(Events::default());
    let book = Arc::new(MeterBook::new(None, Some(events.clone() as Arc<dyn marv_kernel::EventLog>), None));
    let big = Scripted::new(vec![
        vec![
            marv_core::ProviderEvent::TextDelta("Read it all.".into()),
            marv_core::ProviderEvent::Usage(
                marv_core::Usage { prompt_tokens: 50_000, completion_tokens: 10, ..marv_core::Usage::default() },
                marv_core::ReplyEnd::default(),
            ),
            marv_core::ProviderEvent::Done { reason: Some("stop".into()) },
        ],
        say("THE SUMMARY"),
    ]);
    let small = Scripted::new(vec![say("Fixed, from the summary."), say("Again.")]);
    let mut init =
        SessionInit::new(&s.root, ProviderChoice::Factory(Arc::new(Core("big", Arc::new(Windowed(big.clone(), 200_000))))));
    (init.diagnostics, init.checks, init.checkpoints) = (false.into(), false, false);
    init.meters = Some(book);
    init.meter_chat = Some("c1".into());
    let session = MarvSession::new(init).unwrap();
    let mut first = session.send("read the repository", SendOptions::default()).unwrap();
    while first.next().await.is_some() {}
    session
        .configure(marv_session::Configure {
            provider: Some(ProviderChoice::Factory(Arc::new(Core("small", Arc::new(Windowed(small.clone(), 8_000)))))),
            ..marv_session::Configure::default()
        })
        .unwrap();
    let mut second = session.send("now fix it", SendOptions::default()).unwrap();
    while second.next().await.is_some() {}
    // To another core and back before a turn: no switch at all.
    for core in [Core("big", Arc::new(Windowed(big.clone(), 200_000))), Core("small", Arc::new(Windowed(small.clone(), 8_000)))] {
        session
            .configure(marv_session::Configure {
                provider: Some(ProviderChoice::Factory(Arc::new(core))),
                ..marv_session::Configure::default()
            })
            .unwrap();
    }
    let mut third = session.send("and again", SendOptions::default()).unwrap();
    while third.next().await.is_some() {}
    session.close().await;

    assert_eq!(big.requests.lock().unwrap().len(), 2, "the old core read it and summarized it");
    let sent = small.requests.lock().unwrap().clone();
    assert_eq!(sent.len(), 2, "the new core got only the turns");
    let text = format!("{:?}", sent[0].0);
    assert!(text.contains("THE SUMMARY") && !text.contains("Read it all."), "it starts from the summary: {text}");
    let switched: Vec<_> =
        events.0.lock().unwrap().iter().filter(|e| matches!(e, KernelEvent::CoreSwitched { .. })).cloned().collect();
    assert_eq!(
        switched,
        [KernelEvent::CoreSwitched {
            chat: "c1".into(),
            from: "openrouter/big".into(),
            to: "openrouter/small".into(),
            paged_out: true
        }]
    );
}

/// `marv -p` is metered and meets the person's budgets: at a limit it sends nothing, says so on stderr and exits 4;
/// no `--approve` flag (tainted or not) answers a budget's question.
///
/// Proven: removed `marv -p`'s MeterBook in `run_headless` → the run finished (exit 0).
#[test]
fn headless_stops_at_a_budget_and_no_flag_answers_it() {
    let s = Setup::new("headless");
    std::fs::create_dir_all(s.config.join("mesh")).unwrap();
    let path = marv_kernel::budgets_path(&s.config);
    let file = json!({"version": 1, "generation": 1, "budgets": [
        {"scope": "global", "cap": {"usd_micros": 0, "tokens": 0}, "by": "person", "set_at_ms": 1}
    ]});
    std::fs::write(&path, file.to_string()).unwrap();
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let run = marv_guards::headless_steps(
        &s.root,
        &s.config,
        vec![],
        &["--approve", "web", "--approve", "network", "--approve-tainted", "--approve-protected"],
        &[],
    );
    assert_eq!(run.code, Some(4), "{}", run.stderr);
    assert!(run.stderr.contains("reached its $0.00 budget for today: stopped"), "{}", run.stderr);
}
