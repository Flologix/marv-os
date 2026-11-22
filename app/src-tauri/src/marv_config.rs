//! The few settings of Marv's own file (`~/.marv/config.json`, shared with `marv`) the desktop's Settings and Teams
//! show (kernel #53): `smallModel`, and the mesh's `wake`, `radio` and `cards`. Each change is the person's click, saved
//! merged with the file as it is now (`ConfigStore::save_merged`), so a running `marv`'s own save isn't lost.
//! And [`seed`]: the desktop's first-run choice, for a `marv` that has no provider yet.

use std::path::Path;

use marv_provider::ProviderKind;
use marv_session::config::{ConfigStore, FileConfig, RadioSetting};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarvConfig {
    /// The model `marv` in a terminal uses (`model`; read-only here: the desktop keeps its own, and benchmark runs read
    /// this file). The desktop starts on it and says when its own differs.
    pub model: Option<String>,
    /// The model for Marv's side jobs; None: the chat's own.
    pub small_model: Option<String>,
    /// `mesh.wake`: Some(true) any wired message wakes a chat, Some(false) none does, None only a rule's (the default).
    pub mesh_wake: Option<bool>,
    /// `mesh.radio` is on (default off).
    pub mesh_radio: bool,
    /// `mesh.cards` (default on).
    pub mesh_cards: bool,
    /// Why the file can't be read, if it can't (nothing is saved over it then).
    pub problem: Option<String>,
}

/// A change from the page: only the fields it names.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPatch {
    /// Some(""): back to the chat's model.
    pub small_model: Option<String>,
    /// "on", "off" or "rules" (unset: the default).
    pub mesh_wake: Option<String>,
    pub mesh_radio: Option<bool>,
    pub mesh_cards: Option<bool>,
}

fn view(file: Option<&FileConfig>, problem: Option<String>) -> MarvConfig {
    MarvConfig {
        model: file.and_then(|f| f.model.clone()),
        small_model: file.and_then(|f| f.small_model.clone()),
        mesh_wake: file.and_then(|f| f.mesh_wake),
        mesh_radio: file.and_then(|f| f.mesh_radio).is_some_and(|r| r.on),
        mesh_cards: file.and_then(|f| f.mesh_cards).unwrap_or(true),
        problem,
    }
}

pub fn read(dir: &Path) -> MarvConfig {
    match ConfigStore::new(dir).load() {
        Ok(file) => view(file.as_ref(), None),
        Err(e) => view(None, Some(e.to_string())),
    }
}

/// `patch` applied to `file` (a new file when there is none).
pub fn apply(file: Option<&FileConfig>, patch: &ConfigPatch) -> Result<FileConfig, String> {
    let mut next = file.cloned().unwrap_or_default();
    if let Some(model) = &patch.small_model {
        let model = model.trim();
        if model.chars().any(|c| c.is_whitespace() || c.is_control()) || model.len() > 200 {
            return Err(format!("\"{model}\" isn't a model id."));
        }
        next.small_model = (!model.is_empty()).then(|| model.to_string());
    }
    if let Some(wake) = &patch.mesh_wake {
        next.mesh_wake = match wake.as_str() {
            "on" => Some(true),
            "off" => Some(false),
            "rules" => None,
            other => return Err(format!("Unknown wake setting {other}.")),
        };
    }
    if let Some(on) = patch.mesh_radio {
        // Keep the titles choice the file has (`/radio` saves it); a new setting shares titles, as `/radio on` does.
        let titles = next.mesh_radio.map(|r| r.titles).unwrap_or(true);
        next.mesh_radio = Some(RadioSetting { on, titles });
    }
    if let Some(cards) = patch.mesh_cards {
        next.mesh_cards = Some(cards);
    }
    Ok(next)
}

pub fn save(dir: &Path, patch: &ConfigPatch) -> Result<MarvConfig, String> {
    let store = ConfigStore::new(dir);
    // A file that can't be read isn't written over: the person fixes it first.
    let base = store.load().map_err(|e| e.to_string())?;
    let next = apply(base.as_ref(), patch)?;
    let saved = store.save_merged(&next, base.as_ref()).map_err(|e| format!("Couldn't save your Marv settings: {e}"))?;
    Ok(view(Some(&saved), None))
}

/// What the desktop's own first run chose (its `provider`, the key pasted for it, the chat's model or else its default),
/// for a `marv` with no provider of its own yet.
#[derive(Debug, Clone, Default)]
pub struct DesktopChoice<'a> {
    /// The desktop's `provider` ("openrouter", "ollama", "zai", "kimi", "chatgpt"); None: OpenRouter, as `ready` reads it.
    pub provider: Option<&'a str>,
    pub api_key: Option<&'a str>,
    pub zai_api_key: Option<&'a str>,
    pub kimi_api_key: Option<&'a str>,
    /// The chat's own model; empty: none.
    pub model: &'a str,
    /// The desktop's model for new chats (onboarding's or Settings → Model's pick), for a chat with no model of its own:
    /// the first chat's terminal has no settings yet (`build_spec`), so its Marv started on the CLI's default model.
    pub default_model: Option<&'a str>,
}

/// `file` with the desktop's choice filled in, when `marv` has no provider yet; None when there's nothing to do.
/// The desktop's first run saved its choice in its own settings only, so the Marv in a chat's terminal (which reads
/// this file) opened its own setup and asked again for the key just pasted, and the chat's "started on no provider"
/// banner stayed up after it. Only a file without a provider is filled, and only fields it doesn't have: a person's
/// own `marv` setup is never changed.
pub fn seeded(file: Option<&FileConfig>, choice: &DesktopChoice) -> Option<FileConfig> {
    if file.is_some_and(|f| f.provider.is_some()) {
        return None;
    }
    let key = |k: Option<&str>| k.map(str::trim).filter(|k| !k.is_empty()).map(str::to_string);
    let mut next = file.cloned().unwrap_or_default();
    let (provider, api_key) = match choice.provider {
        None | Some("openrouter") => (ProviderKind::OpenRouter, key(choice.api_key)),
        Some("ollama") => (ProviderKind::Ollama, None),
        Some("zai") => (ProviderKind::Zai, key(choice.zai_api_key)),
        Some("kimi") => (ProviderKind::Kimi, key(choice.kimi_api_key)),
        Some("chatgpt") => (ProviderKind::ChatGpt, None),
        Some(_) => return None,
    };
    // A provider that needs a key it hasn't got is left to `marv`'s setup, which asks for it.
    if matches!(provider, ProviderKind::OpenRouter | ProviderKind::Zai | ProviderKind::Kimi) && api_key.is_none() {
        return None;
    }
    next.provider = Some(provider);
    match provider {
        ProviderKind::OpenRouter => next.api_key = next.api_key.or(api_key),
        ProviderKind::Zai => next.zai_api_key = next.zai_api_key.or(api_key),
        ProviderKind::Kimi => next.kimi_api_key = next.kimi_api_key.or(api_key),
        _ => {}
    }
    let model = Some(choice.model).into_iter().chain(choice.default_model).map(str::trim).find(|m| !m.is_empty());
    if next.model.as_deref().is_none_or(|m| m.trim().is_empty())
        && let Some(model) = model
    {
        next.model = Some(model.to_string());
    }
    Some(next)
}

/// [`seeded`], saved: before a chat's Marv starts. Whether the file changed.
pub fn seed(dir: &Path, choice: &DesktopChoice) -> Result<bool, String> {
    let store = ConfigStore::new(dir);
    // A file that can't be read isn't written over: `marv` says what's wrong with it.
    let base = store.load().map_err(|e| e.to_string())?;
    let Some(next) = seeded(base.as_ref(), choice) else { return Ok(false) };
    store.save_merged(&next, base.as_ref()).map_err(|e| format!("Couldn't save your Marv settings: {e}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-or-v1-0123456789abcdef0123456789abcdef";

    fn openrouter<'a>(key: Option<&'a str>, model: &'a str) -> DesktopChoice<'a> {
        DesktopChoice { provider: Some("openrouter"), api_key: key, model, ..DesktopChoice::default() }
    }

    #[test]
    fn a_first_run_choice_fills_a_marv_without_a_provider() {
        let next = seeded(None, &openrouter(Some(KEY), "z-ai/glm-5.3-flash")).unwrap();
        assert_eq!(next.provider, Some(ProviderKind::OpenRouter));
        assert_eq!(next.api_key.as_deref(), Some(KEY));
        assert_eq!(next.model.as_deref(), Some("z-ai/glm-5.3-flash"));
        // What `marv` checks before opening its setup.
        let env = marv_session::config::Env::new();
        let config = marv_session::config::resolve_config(Some(&next), &env);
        assert!(!marv_session::config::needs_setup(Some(&next), &config), "no second setup in the chat's terminal");

        // A file with other settings but no provider keeps them.
        let file = FileConfig { small_model: Some("a/b".into()), ..FileConfig::default() };
        let next = seeded(Some(&file), &openrouter(Some(KEY), "m/n")).unwrap();
        assert_eq!(next.small_model.as_deref(), Some("a/b"));
    }

    #[test]
    fn a_marv_set_up_already_is_never_changed() {
        let file = FileConfig { provider: Some(ProviderKind::Ollama), model: Some("qwen3".into()), ..FileConfig::default() };
        assert!(seeded(Some(&file), &openrouter(Some(KEY), "z-ai/glm-5.3-flash")).is_none());
        // A key of its own (no provider yet) stays its own.
        let file = FileConfig { api_key: Some("sk-or-v1-theirs".into()), ..FileConfig::default() };
        assert_eq!(seeded(Some(&file), &openrouter(Some(KEY), "m/n")).unwrap().api_key.as_deref(), Some("sk-or-v1-theirs"));
    }

    #[test]
    fn a_chat_without_a_model_of_its_own_starts_on_the_desktops() {
        let first = DesktopChoice { default_model: Some("moonshotai/kimi-k3"), ..openrouter(Some(KEY), "") };
        assert_eq!(seeded(None, &first).unwrap().model.as_deref(), Some("moonshotai/kimi-k3"));
        // The chat's own model comes first.
        let own = DesktopChoice { default_model: Some("moonshotai/kimi-k3"), ..openrouter(Some(KEY), "z-ai/glm-5.3-flash") };
        assert_eq!(seeded(None, &own).unwrap().model.as_deref(), Some("z-ai/glm-5.3-flash"));
        // Neither: the file keeps no model, and `marv` takes its own default.
        assert_eq!(seeded(None, &DesktopChoice { default_model: Some(" "), ..openrouter(Some(KEY), "") }).unwrap().model, None);
        // A Marv set up already keeps its model.
        let file = FileConfig { provider: Some(ProviderKind::OpenRouter), model: Some("x/y".into()), ..FileConfig::default() };
        assert!(seeded(Some(&file), &first).is_none());
    }

    #[test]
    fn without_the_key_it_needs_marvs_own_setup_asks() {
        assert!(seeded(None, &openrouter(None, "m/n")).is_none());
        assert!(seeded(None, &openrouter(Some("  "), "m/n")).is_none());
        let kimi = DesktopChoice { provider: Some("kimi"), api_key: Some(KEY), model: "kimi-k3", ..DesktopChoice::default() };
        assert!(seeded(None, &kimi).is_none(), "an OpenRouter key is never sent to another vendor");
        let ollama = DesktopChoice { provider: Some("ollama"), model: "qwen3", ..DesktopChoice::default() };
        assert_eq!(seeded(None, &ollama).unwrap().provider, Some(ProviderKind::Ollama));
        assert!(seeded(None, &DesktopChoice { provider: Some("someday"), ..DesktopChoice::default() }).is_none());
    }

    #[test]
    fn seeded_once_in_a_folder_of_its_own() {
        let dir = std::env::temp_dir().join(format!(
            "marv-config-seed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let choice = openrouter(Some(KEY), "z-ai/glm-5.3-flash");
        assert_eq!(seed(&dir, &choice), Ok(true));
        assert_eq!(seed(&dir, &choice), Ok(false), "the second start finds it set up");
        let file = ConfigStore::new(&dir).load().unwrap().unwrap();
        assert_eq!(file.api_key.as_deref(), Some(KEY));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_patch_changes_only_what_it_names() {
        let mut file = FileConfig { small_model: Some("a/b".into()), mesh_cards: Some(false), ..FileConfig::default() };
        file.mesh_radio = Some(RadioSetting { on: false, titles: false });
        let next = apply(Some(&file), &ConfigPatch { mesh_radio: Some(true), ..ConfigPatch::default() }).unwrap();
        assert_eq!(next.small_model.as_deref(), Some("a/b"));
        assert_eq!(next.mesh_cards, Some(false));
        assert_eq!(next.mesh_radio, Some(RadioSetting { on: true, titles: false }), "the titles choice is kept");
    }

    #[test]
    fn an_empty_small_model_goes_back_to_the_chats_and_a_bad_one_is_refused() {
        let file = FileConfig { small_model: Some("a/b".into()), ..FileConfig::default() };
        let next = apply(Some(&file), &ConfigPatch { small_model: Some(" ".into()), ..ConfigPatch::default() }).unwrap();
        assert_eq!(next.small_model, None);
        assert!(apply(None, &ConfigPatch { small_model: Some("a b".into()), ..ConfigPatch::default() }).is_err());
    }

    #[test]
    fn wake_takes_three_words() {
        let wake =
            |w: &str| apply(None, &ConfigPatch { mesh_wake: Some(w.into()), ..ConfigPatch::default() }).map(|f| f.mesh_wake);
        assert_eq!(wake("on"), Ok(Some(true)));
        assert_eq!(wake("off"), Ok(Some(false)));
        assert_eq!(wake("rules"), Ok(None));
        assert!(wake("maybe").is_err());
    }

    #[test]
    fn saved_and_read_back_in_a_folder_of_its_own() {
        let dir = std::env::temp_dir().join(format!(
            "marv-config-view-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        assert_eq!(read(&dir), view(None, None), "no file: the defaults");
        let saved = save(
            &dir,
            &ConfigPatch { small_model: Some("zai/glm-4.5-air".into()), mesh_wake: Some("off".into()), ..ConfigPatch::default() },
        )
        .unwrap();
        assert_eq!(saved.small_model.as_deref(), Some("zai/glm-4.5-air"));
        assert_eq!(read(&dir), saved);
        assert!(read(&dir).mesh_cards);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
