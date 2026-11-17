//! A harness chat's turns under the person's budgets and its grant's spend cap (runtime-kernel §16; consistency
//! finding H2). Claude Code and the ACP agents run their own loops, so Marv can't check before each of their requests
//! as it does for its own: it checks **before each turn**, with the same rules (`marv_session::allocator`).
//!
//! - At 80% of a budget the person is told once a day; at a limit a person's turn asks (a card: yes covers this turn,
//!   no stops it before anything is sent), and an unattended turn (a timer's, mail's) pauses before it starts.
//! - Each turn's `usage` (its cost as the agent reported it) is charged to the turn's meter, so it's in today's meters,
//!   the event log and Settings → Spending; and, for an unattended turn that carries the chat's grant (as a Marv
//!   chat's timer and rules' mail turns do: `MarvSession::grant_for_turn`), to the grant, so its daily cap holds.
//! - A turn already running isn't cut off at a limit: the agent reports its cost only when the turn ends. The next
//!   turn meets the limit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use marv_session::allocator::{BudgetNotice, Charge, MeterBook, OnBudget, TurnMeter, TurnStart};
use marv_session::grants::ChatGrant;
use marv_tools::Approver;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::host::ChatSpec;

/// What a budget notice tells the page: the notice's text, then its kind (the page's "budget" event).
pub fn budget_notifier(out: UnboundedSender<Value>) -> OnBudget {
    Arc::new(move |notice: BudgetNotice| {
        let kind = match &notice {
            BudgetNotice::Warning(_) => "warning",
            BudgetNotice::Asked { .. } => "asked",
            BudgetNotice::Paused(_) => "paused",
            BudgetNotice::Stopped(_) => "stopped",
        };
        let _ = out.send(json!({"type": "notice", "text": marv_core::printable(&notice.text())}));
        let _ = out.send(json!({"type": "budget", "kind": kind}));
    })
}

/// One harness chat's meter across its turns.
pub struct HarnessBudget {
    book: Option<Arc<MeterBook>>,
    grant: Option<Arc<dyn ChatGrant>>,
    chat: String,
    folder: std::path::PathBuf,
    /// The agent as the meters name it (`<harness>/<agent>`: Claude Code's model isn't Marv's to know).
    core: String,
    /// Whether the turn about to start is unattended (the desktop sets it with a timer's or mail's message).
    woken: Arc<AtomicBool>,
    /// Whether the running turn is: what its Marv OS tool calls see (`HarnessCalls`).
    turn_woken: Arc<AtomicBool>,
    approver: Arc<dyn Approver>,
    notify: OnBudget,
    /// The running turn's meter, and whether the turn carries the grant.
    turn: Mutex<Option<(Arc<TurnMeter>, bool)>>,
}

impl HarnessBudget {
    /// For chat `spec` run by harness `harness` (its id, e.g. `claude-code`). Without meters or a grant (marv-web's
    /// server) it still runs every turn and charges nothing.
    pub fn new(spec: &ChatSpec, harness: &str, approver: Arc<dyn Approver>, out: UnboundedSender<Value>) -> Arc<Self> {
        Arc::new(Self {
            book: spec.meters.clone(),
            grant: spec.grant.clone(),
            chat: spec.chat_id.clone().unwrap_or_default(),
            folder: spec.root.clone(),
            core: format!("{harness}/{harness}"),
            woken: spec.resident.as_ref().map(|r| r.woken.clone()).unwrap_or_default(),
            turn_woken: spec.resident.as_ref().map(|r| r.harness_calls.woken.clone()).unwrap_or_default(),
            approver,
            notify: budget_notifier(out),
            turn: Mutex::new(None),
        })
    }

    /// Whether the turn about to start carries the chat's grant: unattended, live, and for this folder.
    fn carries_grant(&self, woken: bool) -> Option<marv_kernel::GrantId> {
        let grant = self.grant.as_ref().filter(|g| woken && g.covers(&self.folder))?;
        grant.live()
    }

    /// Before a turn's message goes to the agent: its meter starts, and every budget over the chat is checked. False:
    /// it stopped at a limit (the person said no, or it was unattended and paused); nothing may be sent.
    pub async fn begin(&self, stop: &CancellationToken) -> bool {
        let woken = self.woken.swap(false, Ordering::SeqCst);
        self.turn_woken.store(woken, Ordering::SeqCst);
        let grant = self.carries_grant(woken);
        // No meters (marv-web's server): no budgets to meet, and no grant either.
        let Some(book) = &self.book else { return true };
        let meter = book.begin(TurnStart {
            chat: self.chat.clone(),
            folder: self.folder.clone(),
            core: self.core.clone(),
            grant,
            started_by: if woken { marv_kernel::StartedBy::Wake } else { marv_kernel::StartedBy::Person },
        });
        let asks = (!woken).then_some(&self.approver);
        let ok = meter.gate(asks, stop, &self.notify).await;
        if ok {
            *self.turn.lock().unwrap_or_else(|e| e.into_inner()) = Some((meter, grant.is_some()));
        } else {
            meter.finish();
        }
        ok
    }

    /// One of the agent's events: a `usage` is charged (the turn's meter, the grant); a `turn_end` closes the meter.
    pub fn observe(&self, event: &Value) {
        match event["type"].as_str() {
            Some("usage") => {
                let n = |k: &str| event[k].as_u64().unwrap_or(0);
                let cost = event["cost"].as_f64().filter(|c| c.is_finite() && *c >= 0.0);
                let charge = Charge {
                    tokens_in: n("promptTokens"),
                    cached: n("cachedTokens"),
                    out: n("completionTokens"),
                    cost_usd: cost,
                    estimated: false,
                };
                let carried = match self.turn.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    Some((meter, carried)) => {
                        meter.charge(&self.core, charge);
                        *carried
                    }
                    None => false,
                };
                if let Some(grant) = self.grant.as_ref().filter(|_| carried) {
                    grant.spend(cost, charge.tokens_in.saturating_add(charge.out));
                }
            }
            Some("turn_end") => {
                if let Some((meter, _)) = self.turn.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    meter.finish();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marv_kernel::{BudgetScope, BudgetStore, FileBudgets, GrantId, SpendCap};
    use marv_tools::{ApprovalRequest, Decision};

    struct Answer(Decision, Mutex<usize>);
    #[async_trait::async_trait]
    impl Approver for Answer {
        async fn approve(&self, _: ApprovalRequest) -> Decision {
            *self.1.lock().unwrap() += 1;
            self.0.clone()
        }
    }

    #[derive(Default)]
    struct Grant(Mutex<Vec<Option<f64>>>);
    impl ChatGrant for Grant {
        fn live(&self) -> Option<GrantId> {
            Some(GrantId(7))
        }
        fn granted(&self) -> bool {
            true
        }
        fn covers(&self, _: &std::path::Path) -> bool {
            true
        }
        fn used(&self, _: GrantId, _: &str, _: &str, _: u32) {}
        fn spend(&self, cost_usd: Option<f64>, _: u64) {
            self.0.lock().unwrap().push(cost_usd);
        }
    }

    fn usage(cost: f64) -> Value {
        json!({"type": "usage", "promptTokens": 100, "cachedTokens": 0, "completionTokens": 10, "cost": cost})
    }

    /// **H2: a harness chat's turns meet the person's budgets, and its spend counts.** Its turns' cost is in the
    /// meters (Settings → Spending); at a limit a person's turn asks and no stops it, an unattended one pauses without
    /// asking; only an unattended turn's spend goes to the grant, as a Marv chat's does.
    #[tokio::test]
    async fn a_harness_chats_turns_meet_the_budgets_and_charge_the_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BudgetStore::new(&tmp.path().join(".marv"), &tmp.path().join("home")).unwrap();
        let cap = SpendCap { usd_micros: 50_000, tokens: 1_000_000_000 };
        store.set(BudgetScope::Chat("c1".into()), Some(cap), 1).await.unwrap();
        let book = Arc::new(MeterBook::new(None, None, Some(Arc::new(FileBudgets::new(store)))));
        let grant = Arc::new(Grant::default());
        let no = Arc::new(Answer(Decision::No, Mutex::default()));
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let woken = Arc::new(AtomicBool::new(false));
        let budget = HarnessBudget {
            book: Some(book.clone()),
            grant: Some(grant.clone()),
            chat: "c1".into(),
            folder: tmp.path().to_path_buf(),
            core: "claude-code/claude-code".into(),
            woken: woken.clone(),
            turn_woken: Arc::default(),
            approver: no.clone(),
            notify: budget_notifier(out),
            turn: Mutex::new(None),
        };
        let stop = CancellationToken::new();

        // A person's turn under the limit runs; its cost is today's spend, not the grant's.
        assert!(budget.begin(&stop).await);
        budget.observe(&usage(0.06));
        budget.observe(&json!({"type": "turn_end"}));
        assert_eq!(book.spent(&BudgetScope::Chat("c1".into())).usd_micros, 60_000);
        assert!(grant.0.lock().unwrap().is_empty(), "a person's turn never spends the grant");

        // Past the limit: the person is asked, says no, and nothing runs.
        assert!(!budget.begin(&stop).await);
        assert_eq!(*no.1.lock().unwrap(), 1);
        // Unattended: it pauses, asking no one.
        woken.store(true, Ordering::SeqCst);
        assert!(!budget.begin(&stop).await);
        assert_eq!(*no.1.lock().unwrap(), 1, "not asked");
        assert!(!woken.load(Ordering::SeqCst), "the flag is for one turn");
        let mut kinds = Vec::new();
        while let Ok(e) = events.try_recv() {
            kinds.extend(e["kind"].as_str().map(String::from));
        }
        assert_eq!(kinds, ["asked", "paused"]);
    }

    /// An unattended turn under no budget carries the grant: its cost goes to the grant's daily cap.
    #[tokio::test]
    async fn an_unattended_harness_turn_spends_its_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let book = Arc::new(MeterBook::new(None, None, None));
        let grant = Arc::new(Grant::default());
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let budget = HarnessBudget {
            book: Some(book),
            grant: Some(grant.clone()),
            chat: "c1".into(),
            folder: tmp.path().to_path_buf(),
            core: "opencode/opencode".into(),
            woken: Arc::new(AtomicBool::new(true)),
            turn_woken: Arc::default(),
            approver: Arc::new(Answer(Decision::Yes, Mutex::default())),
            notify: budget_notifier(out),
            turn: Mutex::new(None),
        };
        assert!(budget.begin(&CancellationToken::new()).await);
        budget.observe(&usage(0.02));
        assert_eq!(*grant.0.lock().unwrap(), [Some(0.02)]);
    }
}
