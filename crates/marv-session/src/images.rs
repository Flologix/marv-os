//! The images a person's message brings (marv-rs, no Marv equivalent).
//!
//! A message attaches every image file it names: a path the person typed or dragged in, or the one Marv saved when
//! they pasted a screenshot ([`save_pasted`]: the TUI's Ctrl+V, the desktop composer's paste). Only the person's own
//! turn does this, never a woken one: a teammate's message naming `~/.ssh/key.png` mustn't make Marv send a file of
//! the person's anywhere.
//!
//! A model that reads images gets them as an [`ChatTurn::Images`] turn after the message. One that doesn't gets a
//! description instead, written by a model that does ([`VISION_MODEL`], or `visionModel` in the config): the turn
//! still works, and the description says whose words it is.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use marv_core::image::{Image, has_image_extension};
use marv_core::{ChatTurn, ProviderEvent, Usage};
use marv_provider::{Provider, StreamOptions};

/// The most images one message attaches.
pub const MAX_NAMED: usize = 8;

/// The model that describes an image for a model that can't read one, on OpenRouter: small, cheap and good at
/// screenshots (text, layout, what looks broken).
pub const VISION_MODEL: &str = "anthropic/claude-haiku-5.5";

/// What a description is asked for.
pub const DESCRIBE_PROMPT: &str = "Describe this image for a coding agent that can't see it. Say what it shows \
(the app, the page or the window), its layout from top to bottom, all readable text exactly, colours where they \
matter, and anything that looks broken, empty, misaligned, cut off or unfinished. Be specific and factual; don't \
guess at what isn't visible. At most 300 words.";

/// The image paths `text` names, in order, each once: absolute, `~/…`, `file://…` or relative to `root`, ending in an
/// image extension, quoted or not. Whether each is an image is decided when it's read.
pub fn named_paths(text: &str, root: &Path, home: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for word in words(text) {
        let word = word.strip_prefix("file://").unwrap_or(&word).to_string();
        if !has_image_extension(&word) {
            continue;
        }
        let path = if let Some(rest) = word.strip_prefix("~/") {
            home.join(rest)
        } else if Path::new(&word).is_absolute() {
            PathBuf::from(&word)
        } else {
            root.join(&word)
        };
        if !out.contains(&path) {
            out.push(path);
        }
        if out.len() == MAX_NAMED {
            break;
        }
    }
    out
}

/// `text`'s words: split on whitespace, except inside quotes ('…', "…", `…`), with surrounding brackets and trailing
/// punctuation trimmed, and a backslash-escaped space (a dragged path in some terminals) kept.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                out.push(std::mem::take(&mut word));
                quote = None;
            }
            (Some(_), c) => word.push(c),
            (None, '\\') if chars.peek() == Some(&' ') => {
                word.push(' ');
                chars.next();
            }
            (None, '\'' | '"' | '`') if word.is_empty() => quote = Some(c),
            (None, c) if c.is_whitespace() => out.push(std::mem::take(&mut word)),
            (None, c) => word.push(c),
        }
    }
    out.push(word);
    out.into_iter()
        .map(|w| {
            w.trim_start_matches(['(', '[', '<']).trim_end_matches([')', ']', '>', ',', ';', ':', '!', '?', '.']).to_string()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// The images `text` names, read, and a note for each named file that isn't one (missing, too big, not an image).
/// A path that doesn't exist is skipped without a note when it isn't an absolute one: "logo.png" in a sentence may be
/// a file the person wants made.
pub fn load_named(text: &str, root: &Path, home: &Path) -> (Vec<Image>, Vec<String>) {
    let mut images = Vec::new();
    let mut notes = Vec::new();
    for path in named_paths(text, root, home) {
        let shown = shown_path(&path, root, home);
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => continue,
            Err(_) => {
                if path.is_absolute() && !path.starts_with(root) {
                    notes.push(format!("{shown} wasn't attached: no such file."));
                }
                continue;
            }
        }
        if std::fs::metadata(&path).map(|m| m.len() as usize > marv_core::image::MAX_IMAGE_BYTES).unwrap_or(false) {
            notes.push(format!("{shown} wasn't attached: models take images up to 5 MB."));
            continue;
        }
        match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| Image::from_bytes(&bytes, &shown)) {
            Ok(image) => images.push(image),
            Err(e) => notes.push(format!("{shown} wasn't attached: {e}")),
        }
    }
    (images, notes)
}

/// `path` as the transcript names it: project-relative inside the project, `~/…` in the home folder.
pub fn shown_path(path: &Path, root: &Path, home: &Path) -> String {
    if let Ok(rest) = path.strip_prefix(root) {
        return rest.to_string_lossy().into_owned();
    }
    if let Ok(rest) = path.strip_prefix(home) {
        return format!("~/{}", rest.to_string_lossy());
    }
    path.to_string_lossy().into_owned()
}

/// Saves a pasted image's `bytes` under `<config_dir>/pasted/` ([`save_image_in`]) and returns its path, for the
/// message to name.
pub fn save_pasted(config_dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    save_image_in(&config_dir.join("pasted"), bytes)
}

/// Saves image `bytes` in `dir`, named by their hash (the same screenshot twice is one file), readable by the person
/// only. Refuses what isn't an image a model takes.
pub fn save_image_in(dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let image = Image::from_bytes(bytes, "the image")?;
    let ext = match image.media_type.as_str() {
        "image/jpeg" => "jpg",
        other => other.trim_start_matches("image/"),
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't save the image: {e}"))?;
    let path = dir.join(format!("{}.{ext}", short_hash(bytes)));
    if !path.exists() {
        write_private(&path, bytes).map_err(|e| format!("Couldn't save the image: {e}"))?;
    }
    Ok(path)
}

/// `bytes` to `path`, readable by the person only (a screenshot can hold anything), through a temporary file so a
/// half-written image is never read.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let temp = path.with_extension(format!("tmp-{}", marv_fs::random_hex()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
}

/// 16 hex digits of FNV-1a over `bytes`: a name, not a check.
fn short_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// The text a model that can't read images gets in an image's place: the description, and whose it is.
pub fn described(image: &Image, model: &str, description: &str) -> String {
    format!("[Image {}: you can't see images, so {model} described it]\n{}", image.describe(), description.trim())
}

/// The text when no model could describe it.
pub fn undescribed(image: &Image, why: &str) -> String {
    format!(
        "[Image {}: this model can't read images and it couldn't be described ({why}). Say so if you need it.]",
        image.describe()
    )
}

/// [`Vision::on_usage`]: a description request's usage.
pub type OnUsage = Arc<dyn Fn(&Usage) + Send + Sync>;

/// [`Vision::on_image`]: the image, who brought it, and the describing model and its description when there is one.
pub type OnImage = Arc<dyn Fn(&Image, &str, Option<&str>, Option<&str>) + Send + Sync>;

/// How a chat's images reach its model: as images, or as a description another model wrote.
pub struct Vision {
    /// The chat's model reads images (`None` in its model info counts as yes: the provider decides).
    pub sees: bool,
    /// A model that does, and its name, for a chat whose model doesn't. `None`: images are named only.
    pub describer: Option<(Arc<dyn Provider>, String)>,
    /// The description request's usage, for the chat's totals.
    pub on_usage: Option<OnUsage>,
    /// Each image as it's shown or described, for the trace: the image, who brought it ("person" or the tool's call
    /// id), and the description when there is one.
    pub on_image: Option<OnImage>,
}

impl Vision {
    /// `images` for the model: the ones to show, and the text to add instead of the ones it can't see. `from`: who
    /// brought them, for the trace.
    pub async fn take(&self, images: Vec<Image>, from: &str) -> (Vec<Image>, Vec<String>) {
        if self.sees {
            for image in &images {
                if let Some(record) = &self.on_image {
                    record(image, from, None, None);
                }
            }
            return (images, Vec::new());
        }
        let mut texts = Vec::new();
        for image in &images {
            let text = match &self.describer {
                Some((provider, model)) => match self.describe(provider.as_ref(), image).await {
                    Ok(description) => {
                        if let Some(record) = &self.on_image {
                            record(image, from, Some(model), Some(&description));
                        }
                        described(image, model, &description)
                    }
                    Err(e) => undescribed(image, &e),
                },
                None => {
                    undescribed(image, "no vision model is set: add visionModel to the config, or pick a model that reads images")
                }
            };
            texts.push(text);
        }
        (Vec::new(), texts)
    }

    /// One description: [`DESCRIBE_PROMPT`] and the image, no tools.
    async fn describe(&self, provider: &dyn Provider, image: &Image) -> Result<String, String> {
        let history = [ChatTurn::User { text: DESCRIBE_PROMPT.into() }, ChatTurn::Images { images: vec![image.clone()] }];
        let mut stream = provider.stream(&history, StreamOptions::default());
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            match event {
                ProviderEvent::TextDelta(delta) => text.push_str(&delta),
                ProviderEvent::Usage(usage, _) => {
                    if let Some(on_usage) = &self.on_usage {
                        on_usage(&usage);
                    }
                }
                ProviderEvent::Error(e) => return Err(e),
                ProviderEvent::Discard { .. } => text.clear(),
                _ => {}
            }
        }
        let text = marv_core::printable::printable(text.trim());
        if text.is_empty() { Err("the vision model answered nothing".into()) } else { Ok(text) }
    }
}

/// Folds a tool run's images through `vision`: kept to be shown, or replaced by descriptions in its output.
pub async fn tool_images(vision: &Vision, run: &mut marv_agent::agent::ToolRun, call_id: &str) {
    if run.result.images.is_empty() {
        return;
    }
    let images = std::mem::take(&mut run.result.images);
    let (shown, texts) = vision.take(images, call_id).await;
    run.result.images = shown;
    for text in texts {
        run.result.output.push_str("\n\n");
        run.result.output.push_str(&text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x10\0\0\0\x08\x08\x06\0\0\0";

    #[test]
    fn a_message_names_images_by_path_in_every_form_a_person_types_or_drags() {
        let (root, home) = (Path::new("/p/game"), Path::new("/home/me"));
        let text = "look at shots/a.png and ~/Pictures/b.JPG, also '/tmp/my shot.webp' (file:///tmp/c.gif). \
                    /tmp/d\\ e.png then notes.txt and shots/a.png again";
        assert_eq!(
            named_paths(text, root, home),
            [
                PathBuf::from("/p/game/shots/a.png"),
                PathBuf::from("/home/me/Pictures/b.JPG"),
                PathBuf::from("/tmp/my shot.webp"),
                PathBuf::from("/tmp/c.gif"),
                PathBuf::from("/tmp/d e.png"),
            ]
        );
        assert!(named_paths("make a logo.pngx", root, home).is_empty());
    }

    #[test]
    fn named_images_are_read_and_what_isnt_one_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("shot.png"), PNG).unwrap();
        std::fs::write(root.join("fake.png"), b"text").unwrap();
        let outside = dir.path().join("out.png");
        std::fs::write(&outside, PNG).unwrap();
        let text = format!("see shot.png, fake.png, {} and {}", outside.display(), dir.path().join("gone.png").display());
        let (images, notes) = load_named(&text, &root, dir.path());
        assert_eq!(images.iter().map(|i| i.source.as_str()).collect::<Vec<_>>(), ["shot.png", "~/out.png"]);
        assert_eq!(
            notes,
            [
                "fake.png wasn't attached: fake.png isn't a PNG, JPEG, GIF or WebP image.",
                "~/gone.png wasn't attached: no such file."
            ]
        );
        // A file the person wants made isn't a missing attachment.
        assert_eq!(load_named("make logo.png", &root, dir.path()), (vec![], vec![]));
    }

    #[test]
    fn a_pasted_image_is_saved_once_by_its_hash_and_text_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let first = save_pasted(dir.path(), PNG).unwrap();
        assert!(first.starts_with(dir.path().join("pasted")) && first.extension().unwrap() == "png");
        assert_eq!(save_pasted(dir.path(), PNG).unwrap(), first);
        assert_eq!(std::fs::read(&first).unwrap(), PNG);
        assert!(save_pasted(dir.path(), b"hello").is_err());
    }
}
