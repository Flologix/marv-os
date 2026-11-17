//! Cores: which provider and model does a piece of work. New in marv-rs.
//!
//! A core is a provider and a model. The person picks the chat's (`/model`, the desktop's picker); an agent type's
//! `model:` picks its subagent's; and `smallModel` (the person's config) picks the one for Marv's own side jobs: the
//! next-step suggestion. Unset, everything runs on the chat's core, as before.
//!
//! A model name may say which provider it's on: `zai/glm-4.6`, `chatgpt/gpt-5`, `ollama/qwen3.5:9b`, or
//! `openrouter/<id>` (on an OpenRouter chat the whole name is kept: OpenRouter has ids of its own that start so,
//! `openrouter/auto`). Anything else is a model on the chat's provider. Another provider's core is made by the
//! [`CoreMaker`] the client gives (from the person's config: its keys, its sign-in), and only for a provider set up
//! there.
//!
//! **Another provider only from the person's own files.** A project's `.marv/agents` may name models on the chat's
//! provider only ([`resolve_agent_core`]): a file in a repository choosing *where your code is sent* would be a way
//! to send it somewhere the person never picked. The person's `~/.marv/agents` and `smallModel` may name any provider
//! the person set up. A project's agent type saying `model: small` gets the small model only when that is on the
//! chat's provider too (otherwise the chat's own model).

use std::sync::Arc;

use marv_provider::{Provider, ProviderFactory, ProviderKind};

use crate::agents::{AgentSource, AgentType};

/// The agent types' (and `smallModel`'s) word for the small model.
pub const SMALL: &str = "small";

/// Makes another provider's core from the person's config: `None` when that provider isn't set up (no key, no
/// sign-in).
pub type CoreMaker = Arc<dyn Fn(ProviderKind, &str) -> Option<Arc<dyn ProviderFactory>> + Send + Sync>;

/// What a model name says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreName {
    /// A model on the chat's provider.
    Same(String),
    /// A model on another provider.
    Other(ProviderKind, String),
}

/// The provider a factory's id names ("openrouter", "zai"…); `None` for one of the SDK user's own ("custom").
pub fn kind_of(id: &str) -> Option<ProviderKind> {
    [ProviderKind::OpenRouter, ProviderKind::Ollama, ProviderKind::Zai, ProviderKind::ChatGpt].into_iter().find(|k| k.id() == id)
}

/// Reads a model name against the chat's provider (`None`: a provider of the SDK user's own, where every name is
/// its model's).
pub fn parse_core(name: &str, chat: Option<ProviderKind>) -> CoreName {
    let Some(chat) = chat else { return CoreName::Same(name.to_string()) };
    let prefixed = [ProviderKind::Ollama, ProviderKind::Zai, ProviderKind::ChatGpt, ProviderKind::OpenRouter]
        .into_iter()
        .find_map(|k| name.strip_prefix(k.id()).and_then(|r| r.strip_prefix('/')).map(|rest| (k, rest)));
    match prefixed {
        // OpenRouter's own `openrouter/…` ids stay whole on an OpenRouter chat.
        Some((ProviderKind::OpenRouter, _)) if chat == ProviderKind::OpenRouter => CoreName::Same(name.to_string()),
        Some((kind, rest)) if !rest.is_empty() => {
            if kind == chat {
                CoreName::Same(rest.to_string())
            } else {
                CoreName::Other(kind, rest.to_string())
            }
        }
        _ => CoreName::Same(name.to_string()),
    }
}

/// A core, made: its factory (what its requests are counted against) and its provider.
#[derive(Clone)]
pub struct Core {
    pub factory: Arc<dyn ProviderFactory>,
    pub provider: Arc<dyn Provider>,
    /// `provider/model`, as the person reads it ("zai/glm-4.6").
    pub name: String,
    /// On another provider than the chat's.
    pub other: bool,
}

/// What the cores come from, for one chat.
#[derive(Clone)]
pub struct Cores {
    /// The chat's factory and provider.
    pub factory: Arc<dyn ProviderFactory>,
    pub provider: Arc<dyn Provider>,
    /// The person's `smallModel`, if set.
    pub small: Option<String>,
    pub maker: Option<CoreMaker>,
}

impl Cores {
    /// The chat's own core.
    pub fn chat(&self) -> Core {
        Core {
            factory: self.factory.clone(),
            provider: self.provider.clone(),
            name: format!("{}/{}", self.factory.id(), self.factory.model()),
            other: false,
        }
    }

    /// The core a model name picks. `personal`: the name is the person's own (their agent file, their config), so it
    /// may be on another provider.
    pub fn named(&self, name: &str, personal: bool) -> Result<Core, String> {
        match parse_core(name, kind_of(self.factory.id())) {
            CoreName::Same(model) => Ok(Core {
                provider: self.factory.make(Some(&model), None),
                name: format!("{}/{model}", self.factory.id()),
                factory: self.factory.clone(),
                other: false,
            }),
            CoreName::Other(kind, model) => {
                if !personal {
                    return Err(format!(
                        "{name} is on {}, not this chat's provider: only your own agent types (~/.marv/agents) may use another provider.",
                        kind.label()
                    ));
                }
                let factory = self
                    .maker
                    .as_ref()
                    .and_then(|make| make(kind, &model))
                    .ok_or_else(|| format!("{name} needs {}, which isn't set up here.", kind.label()))?;
                Ok(Core { provider: factory.make(None, None), name: format!("{}/{model}", kind.id()), factory, other: true })
            }
        }
    }

    /// The small model's core (Marv's side jobs), or the chat's when none is set. A small model that can't be made
    /// (its provider isn't set up any more) falls back to the chat's too: a side job never fails the chat.
    pub fn small(&self) -> Core {
        self.small.as_deref().and_then(|name| self.named(name, true).ok()).unwrap_or_else(|| self.chat())
    }

    /// The core for an agent type's subagent ([`resolve_agent_core`]).
    pub fn for_agent(&self, agent: &AgentType) -> Result<Core, String> {
        resolve_agent_core(self, agent)
    }
}

/// An agent type's core: none named, the chat's; `small`, the small model (a project's type only when it is on the
/// chat's provider); any other name, that core, on another provider only for the person's own types.
pub fn resolve_agent_core(cores: &Cores, agent: &AgentType) -> Result<Core, String> {
    let personal = agent.source != AgentSource::Project;
    match agent.model.as_deref() {
        None => Ok(cores.chat()),
        Some(SMALL) => match cores.small.as_deref() {
            None => Ok(cores.chat()),
            Some(small) if personal => Ok(cores.named(small, true).unwrap_or_else(|_| cores.chat())),
            Some(small) => Ok(cores.named(small, false).unwrap_or_else(|_| cores.chat())),
        },
        Some(model) => cores.named(model, personal).map_err(|e| format!("The {} agent type: {e}", agent.name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn a_prefix_names_the_provider_and_the_chats_own_is_dropped() {
        let or = Some(ProviderKind::OpenRouter);
        assert_eq!(parse_core("z-ai/glm-4.6", or), CoreName::Same("z-ai/glm-4.6".into()));
        assert_eq!(parse_core("zai/glm-4.6", or), CoreName::Other(ProviderKind::Zai, "glm-4.6".into()));
        assert_eq!(parse_core("chatgpt/gpt-5", or), CoreName::Other(ProviderKind::ChatGpt, "gpt-5".into()));
        assert_eq!(parse_core("openrouter/auto", or), CoreName::Same("openrouter/auto".into()), "OpenRouter's own id");
        let zai = Some(ProviderKind::Zai);
        assert_eq!(parse_core("zai/glm-4.6", zai), CoreName::Same("glm-4.6".into()));
        assert_eq!(
            parse_core("openrouter/anthropic/claude-sonnet-5.5", zai),
            CoreName::Other(ProviderKind::OpenRouter, "anthropic/claude-sonnet-5.5".into())
        );
        assert_eq!(parse_core("zai/", or), CoreName::Same("zai/".into()), "no model: a name, not a provider");
        assert_eq!(parse_core("zai/glm-4.6", None), CoreName::Same("zai/glm-4.6".into()), "an SDK provider's own");
    }

    struct Named(&'static str, String);
    impl ProviderFactory for Named {
        fn id(&self) -> &str {
            self.0
        }
        fn model(&self) -> &str {
            &self.1
        }
        fn make(&self, model: Option<&str>, _info: Option<&marv_provider::ModelInfo>) -> Arc<dyn Provider> {
            Arc::new(marv_provider::FailingProvider {
                name: format!("{}/{}", self.0, model.unwrap_or(&self.1)),
                message: String::new(),
            })
        }
    }

    fn cores(small: Option<&str>, made: Arc<Mutex<Vec<String>>>) -> Cores {
        let factory: Arc<dyn ProviderFactory> = Arc::new(Named("openrouter", "big".into()));
        Cores {
            provider: factory.make(None, None),
            factory,
            small: small.map(String::from),
            maker: Some(Arc::new(move |kind, model| {
                made.lock().unwrap().push(format!("{}/{model}", kind.id()));
                (kind == ProviderKind::Zai).then(|| Arc::new(Named("zai", model.to_string())) as Arc<dyn ProviderFactory>)
            })),
        }
    }

    fn agent(model: &str, source: AgentSource) -> AgentType {
        AgentType { model: Some(model.into()), source, ..crate::agents::general_purpose() }
    }

    #[test]
    fn only_the_persons_own_types_reach_another_provider() {
        let made = Arc::new(Mutex::new(Vec::new()));
        let cores = cores(None, made.clone());
        let personal = resolve_agent_core(&cores, &agent("zai/glm-4.6", AgentSource::Personal)).unwrap();
        assert_eq!((personal.name.as_str(), personal.other), ("zai/glm-4.6", true));
        let project = resolve_agent_core(&cores, &agent("zai/glm-4.6", AgentSource::Project)).err().unwrap();
        assert!(project.contains("only your own agent types"), "{project}");
        assert_eq!(*made.lock().unwrap(), ["zai/glm-4.6"], "the project's type made no core");
        let same = resolve_agent_core(&cores, &agent("z-ai/glm-4.6", AgentSource::Project)).unwrap();
        assert_eq!((same.name.as_str(), same.other), ("openrouter/z-ai/glm-4.6", false));
        let unset = resolve_agent_core(&cores, &agent("chatgpt/gpt-5", AgentSource::Personal)).err().unwrap();
        assert!(unset.contains("ChatGPT, which isn't set up"), "{unset}");
    }

    #[test]
    fn small_is_the_small_model_or_the_chats_and_a_projects_small_stays_on_the_chats_provider() {
        let made = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(cores(None, made.clone()).small().name, "openrouter/big");
        assert_eq!(cores(Some("qwen/qwen3-8b"), made.clone()).small().name, "openrouter/qwen/qwen3-8b");
        let other = cores(Some("zai/glm-4.5-air"), made.clone());
        assert_eq!(other.small().name, "zai/glm-4.5-air");
        assert_eq!(resolve_agent_core(&other, &agent(SMALL, AgentSource::Personal)).unwrap().name, "zai/glm-4.5-air");
        assert_eq!(
            resolve_agent_core(&other, &agent(SMALL, AgentSource::Project)).unwrap().name,
            "openrouter/big",
            "a project's `small` never moves to another provider"
        );
        assert_eq!(cores(Some("chatgpt/x"), made).small().name, "openrouter/big", "not set up: the chat's");
    }
}
