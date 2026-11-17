//! The few settings of Marv's own file (`~/.marv/config.json`, shared with `marv`) the desktop's Settings and Teams
//! show (kernel #53): `smallModel`, and the mesh's `wake`, `radio` and `cards`. Each change is the person's click, saved
//! merged with the file as it is now (`ConfigStore::save_merged`), so a running `marv`'s own save isn't lost.

use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::*;

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
