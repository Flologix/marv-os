//! Cores (`marv_session::cores`): an agent type may run its subagent on another provider only when it's the person's
//! own (`~/.marv/agents`). A project's `.marv/agents` names models on the chat's provider only: a file in a repository
//! choosing where your code is sent would send it somewhere the person never picked.
//!
//! A real session with a scripted model on a factory that says it's OpenRouter, the agent types loaded from real
//! folders. Every guard says how it was proven: "reverted X → this test failed".

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use marv_guards::{Scripted, TempDir, say, tool_call, use_tools};
use marv_provider::{Provider, ProviderFactory, ProviderKind};
use marv_session::session::SessionEvent;
use marv_session::{MarvSession, ProviderChoice, SendOptions, SessionInit};
use serde_json::json;

/// A factory that says it is `id`, every model on one scripted provider.
struct Named(&'static str, Arc<Scripted>);
impl ProviderFactory for Named {
    fn id(&self) -> &str {
        self.0
    }
    fn model(&self) -> &str {
        "chat-model"
    }
    fn make(&self, _model: Option<&str>, _info: Option<&marv_provider::ModelInfo>) -> Arc<dyn Provider> {
        self.1.clone()
    }
}

const REVIEWER: &str = "---\nname: {name}\ndescription: Reviews.\nmodel: zai/glm-4.6\n---\nYou review.";

/// Runs one turn whose model starts the `name` agent type; the subagent's own model is `other`. What the agent tool
/// returned, and which cores the maker was asked for.
async fn start(name: &str, project: bool) -> (String, Vec<String>, usize) {
    let dir = TempDir::new(&format!("cores-{name}"));
    let (root, home) = (dir.path().join("project"), dir.path().join("home"));
    let file = REVIEWER.replace("{name}", name);
    let base = if project { &root } else { &home };
    std::fs::create_dir_all(base.join(".marv/agents")).unwrap();
    std::fs::write(base.join(format!(".marv/agents/{name}.md")), file).unwrap();
    let (agents, problems) = marv_session::agents::load_agents(Some(&root), Some(&home));
    assert!(problems.is_empty(), "{problems:?}");

    let chat = Scripted::new(vec![
        use_tools(vec![tool_call("a1", "agent", json!({"description": "review", "prompt": "review the diff", "type": name}))]),
        say("Done."),
    ]);
    let other = Scripted::new(vec![say("Reviewed on Z.ai.")]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut init = SessionInit::new(&root, ProviderChoice::Factory(Arc::new(Named("openrouter", chat.clone()))));
    (init.diagnostics, init.checks, init.checkpoints) = (false.into(), false, false);
    init.agents = agents;
    let (seen, zai) = (asked.clone(), other.clone());
    init.cores = Some(Arc::new(move |kind: ProviderKind, model: &str| {
        seen.lock().unwrap().push(format!("{}/{model}", kind.id()));
        Some(Arc::new(Named("zai", zai.clone())) as Arc<dyn ProviderFactory>)
    }));
    let session = MarvSession::new(init).unwrap();
    let mut turn = session.send("review it", SendOptions::default()).unwrap();
    let mut output = String::new();
    while let Some(event) = turn.next().await {
        if let SessionEvent::Loop(marv_agent::LoopEvent::ToolEnd { run, .. }) = event {
            output = run.result.output.clone();
        }
    }
    let other_requests = other.requests.lock().unwrap().len();
    let asked = asked.lock().unwrap().clone();
    (output, asked, other_requests)
}

/// A project's agent type naming another provider's model is refused before anything is made or sent; the same file
/// in the person's own `~/.marv/agents` runs there.
///
/// Proven: removed the `if !personal` refusal in `Cores::named` → the project's type made a Z.ai core and its subagent
/// ran on it.
#[tokio::test]
async fn a_projects_agent_type_cant_name_another_provider() {
    let (output, asked, sent) = start("reviewer", true).await;
    assert!(output.contains("only your own agent types (~/.marv/agents) may use another provider"), "{output}");
    assert!(asked.is_empty(), "no core was made: {asked:?}");
    assert_eq!(sent, 0, "nothing reached the other provider");

    let (output, asked, sent) = start("mine", false).await;
    assert!(output.contains("Reviewed on Z.ai."), "{output}");
    assert_eq!(asked, ["zai/glm-4.6"]);
    assert_eq!(sent, 1);
}
