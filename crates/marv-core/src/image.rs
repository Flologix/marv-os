//! Images the model is shown (marv-rs, no Marv equivalent): a screenshot the person pasted, a file `read_file` opened.
//!
//! An image goes into the history as a [`crate::ChatTurn::Images`] turn after the user message or the step's tool
//! results that brought it, its bytes base64 in the saved session. Providers fold it into the message format they
//! speak (an OpenAI `image_url` part, a Responses `input_image`, Ollama's `images`); after tool results it becomes a
//! user message of its own, since no API takes images in a tool result everywhere.
//!
//! Only the newest [`IMAGES_SENT`] images go with a request ([`shown`]): an older one is named in a line of text
//! ([`placeholder`]), so a long session of screenshots doesn't pay for every one on every step. The cut moves only
//! when a new image arrives, so the prompt cache misses once per image, never per step.

use serde::{Deserialize, Serialize};

use crate::ChatTurn;

/// How many of the newest images a request carries.
pub const IMAGES_SENT: usize = 4;
/// The providers' per-image limit (Anthropic's 5 MB, the smallest).
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// What one image counts for in the context estimate: about what a 1000×1000 screenshot costs most vision models.
pub const IMAGE_TOKENS: u64 = 1_500;

/// One image: its bytes, base64, and what they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    #[serde(rename = "mediaType")]
    pub media_type: String,
    /// The bytes, standard base64.
    pub data: String,
    /// Where it came from, as the model and the transcript name it: the path, or "pasted image".
    pub source: String,
}

impl Image {
    /// `bytes` as an image from `source`, if they are one a model takes and small enough to send.
    pub fn from_bytes(bytes: &[u8], source: &str) -> Result<Self, String> {
        let Some(media_type) = media_type(bytes) else {
            return Err(format!("{source} isn't a PNG, JPEG, GIF or WebP image."));
        };
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "{source} is {:.1} MB; models take images up to 5 MB. Make it smaller (a smaller window or a crop) and try again.",
                bytes.len() as f64 / 1_048_576.0
            ));
        }
        use base64::Engine;
        Ok(Self {
            media_type: media_type.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            source: source.to_string(),
        })
    }

    /// The bytes back.
    pub fn bytes(&self) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(&self.data).unwrap_or_default()
    }

    /// `data:image/png;base64,…`, for the APIs that take a URL.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }

    /// One line about it: "shot.png (800×600 PNG, 91 KB)".
    pub fn describe(&self) -> String {
        let bytes = self.bytes();
        let kind = self.media_type.trim_start_matches("image/").to_uppercase();
        let size = format!("{} KB", bytes.len().div_ceil(1024));
        match dimensions(&bytes) {
            Some((w, h)) => format!("{} ({w}×{h} {kind}, {size})", self.source),
            None => format!("{} ({kind}, {size})", self.source),
        }
    }
}

/// The image type `bytes` start with, if a model takes it.
pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Whether `path`'s extension is an image one ([`media_type`] decides by the bytes).
pub fn has_image_extension(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp"].iter().any(|ext| lower.ends_with(ext))
}

/// Width and height from the header, when the format says them where we look.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |i: usize| Some(u32::from_be_bytes(bytes.get(i..i + 4)?.try_into().ok()?));
    let le16 = |i: usize| Some(u16::from_le_bytes(bytes.get(i..i + 2)?.try_into().ok()?) as u32);
    let be16 = |i: usize| Some(u16::from_be_bytes(bytes.get(i..i + 2)?.try_into().ok()?) as u32);
    match media_type(bytes)? {
        "image/png" => Some((be32(16)?, be32(20)?)),
        "image/gif" => Some((le16(6)?, le16(8)?)),
        "image/webp" => match bytes.get(12..16)? {
            b"VP8X" => {
                let le24 = |i: usize| Some(u32::from_le_bytes([*bytes.get(i)?, *bytes.get(i + 1)?, *bytes.get(i + 2)?, 0]));
                Some((le24(24)? + 1, le24(27)? + 1))
            }
            b"VP8 " => Some((le16(26)? & 0x3FFF, le16(28)? & 0x3FFF)),
            b"VP8L" => {
                let b = bytes.get(21..25)?;
                let bits = u32::from_le_bytes(b.try_into().ok()?);
                Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
            }
            _ => None,
        },
        "image/jpeg" => {
            // The first start-of-frame marker holds the size.
            let mut i = 2;
            while i + 9 < bytes.len() {
                if bytes[i] != 0xFF {
                    return None;
                }
                let marker = bytes[i + 1];
                let length = be16(i + 2)? as usize;
                if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                    return Some((be16(i + 7)?, be16(i + 5)?));
                }
                i += 2 + length;
            }
            None
        }
        _ => None,
    }
}

/// For each turn of `history`, whether a request carries its images: true for the [`ChatTurn::Images`] turns holding
/// the newest [`IMAGES_SENT`] images (a turn is carried whole), false for older ones and every other turn.
pub fn shown(history: &[ChatTurn]) -> Vec<bool> {
    let mut out = vec![false; history.len()];
    let mut left = IMAGES_SENT;
    for (i, turn) in history.iter().enumerate().rev() {
        if let ChatTurn::Images { images } = turn {
            if left == 0 {
                break;
            }
            out[i] = true;
            left = left.saturating_sub(images.len());
        }
    }
    out
}

/// The line an image no longer carried is named by.
pub fn placeholder(image: &Image) -> String {
    format!("[Image {}: no longer attached; open it again to look at it.]", image.source)
}

/// What an [`ChatTurn::Images`] turn says as text, for a summary or a text-only request.
pub fn as_text(images: &[Image]) -> String {
    images.iter().map(|i| format!("[Image {}]", i.describe())).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0]);
        b
    }

    #[test]
    fn an_image_is_known_by_its_bytes_and_says_its_size() {
        let image = Image::from_bytes(&png(800, 600), "shot.png").unwrap();
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.bytes(), png(800, 600));
        assert_eq!(image.describe(), "shot.png (800×600 PNG, 1 KB)");
        assert!(image.data_url().starts_with("data:image/png;base64,iVBORw0KGgo"));
        let gif = b"GIF89a\x40\x01\xf0\x00".to_vec();
        assert_eq!(dimensions(&gif), Some((320, 240)));
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 11, 8, 0, 100, 0, 200, 3, 0, 0];
        assert_eq!(dimensions(&jpeg), Some((200, 100)));
        assert!(Image::from_bytes(b"hello", "notes.png").unwrap_err().contains("isn't a PNG"));
        let mut big = png(1, 1);
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        assert!(Image::from_bytes(&big, "big.png").unwrap_err().contains("up to 5 MB"));
        assert!(has_image_extension("a/B.JPEG") && !has_image_extension("a.pngx"));
    }

    #[test]
    fn a_request_carries_only_the_newest_images() {
        let image = |n: usize| Image { media_type: "image/png".into(), data: String::new(), source: format!("{n}.png") };
        let images = |ns: &[usize]| ChatTurn::Images { images: ns.iter().map(|&n| image(n)).collect() };
        let user = ChatTurn::User { text: "look".into() };
        let history = [images(&[1, 2]), user.clone(), images(&[3]), images(&[4, 5]), user, images(&[6])];
        // 6, then 4+5, then 3 make four: 1+2 are named only.
        assert_eq!(shown(&history), [false, false, true, true, false, true]);
        assert_eq!(placeholder(&image(1)), "[Image 1.png: no longer attached; open it again to look at it.]");
    }
}
