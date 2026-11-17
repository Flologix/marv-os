//! Marv desktop's side of `delegate` ([`marv_session::delegate`]): a chat's agent hands a task to another agent, which
//! starts in a new chat in the same folder (and worktree), unattended so inside the outer sandbox, with the task typed
//! in once it's ready. [`Delegations`] follows each delegate's first turn after its task (its hooks' reply where its
//! agent has them, else its terminal going quiet after it worked) for `delegate_wait`.
//!
//! - A delegate can't delegate in turn (one level: no chain of agents starting agents).
//! - A chat sees and stops only its own delegates; the delegate's chat stays (the person can open it, and its changes
//!   are in the folder).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use marv_kernel::AgentState;
use marv_session::delegate::{DelegateStatus, Delegated, Delegates};
use tauri::{AppHandle, Manager};

/// How long after a delegate goes quiet its reply may still come (Claude Code's `Stop` hook, read after the turn).
const REPLY_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Default, Clone)]
struct Run {
    parent: String,
    /// Its task went into its terminal.
    typed: bool,
    /// It worked after its task went in.
    worked: bool,
    done_at: Option<Instant>,
    reply: Option<String>,
}

/// Every delegate this run, by its chat's id.
#[derive(Default)]
pub struct Delegations {
    runs: Mutex<HashMap<String, Run>>,
}

impl Delegations {
    fn runs(&self) -> std::sync::MutexGuard<'_, HashMap<String, Run>> {
        self.runs.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn started(&self, child: &str, parent: &str) {
        self.runs().insert(child.to_string(), Run { parent: parent.to_string(), ..Run::default() });
    }

    /// Text went into chat `id`'s terminal (its task, for a delegate).
    pub fn typed(&self, id: &str) {
        if let Some(run) = self.runs().get_mut(id) {
            run.typed = true;
        }
    }

    /// Chat `id`'s agent changed state: a delegate's turn ends when it goes idle after working on its task.
    pub fn state(&self, id: &str, state: AgentState) {
        let mut runs = self.runs();
        let Some(run) = runs.get_mut(id).filter(|r| r.typed && r.done_at.is_none()) else { return };
        match state {
            AgentState::Busy => run.worked = true,
            AgentState::Idle if run.worked => run.done_at = Some(Instant::now()),
            _ => {}
        }
    }

    /// Chat `id`'s turn ended with this reply.
    pub fn reply(&self, id: &str, text: &str) {
        if let Some(run) = self.runs().get_mut(id).filter(|r| r.typed) {
            run.reply = Some(text.to_string());
            run.done_at.get_or_insert_with(Instant::now);
        }
    }

    /// Delegate `id` of chat `parent`, as `delegate_wait` sees it (`running`: its terminal runs; `asking`: its agent
    /// waits for the person).
    fn status(&self, parent: &str, id: &str, running: bool, asking: bool) -> Result<DelegateStatus, String> {
        let runs = self.runs();
        let run = runs.get(id).filter(|r| r.parent == parent).ok_or("That isn't one of this chat's delegates.")?;
        Ok(match run.done_at {
            Some(_) if run.reply.is_some() => DelegateStatus::Done(run.reply.clone()),
            Some(at) if at.elapsed() >= REPLY_GRACE || !running => DelegateStatus::Done(None),
            _ if !running && run.typed => DelegateStatus::Ended,
            _ if asking => DelegateStatus::NeedsPerson,
            _ => DelegateStatus::Working,
        })
    }

    fn owns(&self, parent: &str, id: &str) -> bool {
        self.runs().get(id).is_some_and(|r| r.parent == parent)
    }
}

/// Chat `chat`'s delegates, for its mesh tools.
pub struct DesktopDelegates {
    pub app: AppHandle,
    pub chat: String,
}

#[async_trait::async_trait]
impl Delegates for DesktopDelegates {
    fn agents(&self) -> Vec<(String, String)> {
        marv_web::terminal_agents::all()
            .into_iter()
            .filter(|a| a.kind != marv_web::terminal_agents::Kind::Shell)
            .filter(|a| a.id == marv_web::terminal_agents::MARV || a.installed().is_some())
            .map(|a| (a.id.to_string(), a.title.to_string()))
            .collect()
    }

    async fn start(&self, agent: &str, task: &str) -> Result<Delegated, String> {
        let to = marv_web::terminal_agents::find(agent)
            .filter(|a| a.kind != marv_web::terminal_agents::Kind::Shell)
            .filter(|a| a.id == marv_web::terminal_agents::MARV || a.installed().is_some())
            .ok_or_else(|| format!("\"{}\" isn't an agent installed here.", marv_core::printable(agent)))?;
        let child = crate::start_delegate(&self.app, &self.chat, to)?;
        let desktop = self.app.state::<crate::Desktop>();
        // Typed once its agent is ready (`Delegations::typed` when it goes in).
        desktop.live.type_when_ready(&child, task.to_string(), true);
        Ok(Delegated { id: child, agent: to.title.to_string() })
    }

    async fn status(&self, id: &str) -> Result<DelegateStatus, String> {
        let running = self.app.state::<crate::terminals::Terminals>().running(&format!("chat:{id}"));
        let desktop = self.app.state::<crate::Desktop>();
        let asking = desktop.live.states().iter().any(|s| s.id == id && s.waiting > 0);
        self.app.state::<std::sync::Arc<Delegations>>().status(&self.chat, id, running, asking)
    }

    async fn cancel(&self, id: &str) -> Result<String, String> {
        if !self.app.state::<std::sync::Arc<Delegations>>().owns(&self.chat, id) {
            return Err("That isn't one of this chat's delegates.".into());
        }
        self.app.state::<crate::terminals::Terminals>().close_key(&format!("chat:{id}"));
        self.app.state::<crate::Desktop>().live.terminal_closed(id);
        Ok("Stopped: its terminal ended. Its chat and what it changed stay.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A delegate's turn is the one after its task: starting up (drawing, then quiet) isn't it; its reply ends it; an
    /// agent without one is done once it goes quiet after working; another chat can't see it.
    #[test]
    fn a_delegates_turn_is_the_one_after_its_task() {
        let d = Delegations::default();
        d.started("c", "p");
        d.state("c", AgentState::Busy);
        d.state("c", AgentState::Idle);
        assert_eq!(d.status("p", "c", true, false), Ok(DelegateStatus::Working), "its start isn't its turn");
        d.typed("c");
        d.state("c", AgentState::Busy);
        assert_eq!(d.status("p", "c", true, true), Ok(DelegateStatus::NeedsPerson));
        d.reply("c", "Done: 3 tests added");
        assert_eq!(d.status("p", "c", true, false), Ok(DelegateStatus::Done(Some("Done: 3 tests added".into()))));
        assert!(d.status("q", "c", true, false).is_err(), "another chat's");

        d.started("o", "p");
        d.typed("o");
        d.state("o", AgentState::Busy);
        d.state("o", AgentState::Idle);
        assert_eq!(d.status("p", "o", false, false), Ok(DelegateStatus::Done(None)), "quiet after working, terminal gone");
        d.started("e", "p");
        d.typed("e");
        assert_eq!(d.status("p", "e", false, false), Ok(DelegateStatus::Ended));
    }
}
