//! Toolkit- and protocol-independent clipboard types and service contracts.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_normalization::UnicodeNormalization;

pub type ClipId = i64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Representation {
    pub target: String,
    pub mime: Option<String>,
    pub bytes: Arc<[u8]>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Offer {
    pub representations: Vec<Representation>,
}

impl Offer {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            representations: vec![Representation {
                target: "UTF8_STRING".into(),
                mime: Some("text/plain;charset=utf-8".into()),
                bytes: Arc::from(text.into().into_bytes()),
            }],
        }
    }

    pub fn size(&self) -> usize {
        self.representations.iter().map(|r| r.bytes.len()).sum()
    }

    pub fn text_content(&self) -> Option<String> {
        let r = self.representations.iter().find(|r| is_text(&r.target))?;
        if r.target == "STRING" {
            Some(r.bytes.iter().map(|&b| char::from(b)).collect())
        } else {
            Some(
                String::from_utf8_lossy(&r.bytes)
                    .trim_end_matches('\0')
                    .to_owned(),
            )
        }
    }

    pub fn digest(&self) -> String {
        let mut formats = BTreeMap::new();
        for r in &self.representations {
            formats.insert(r.mime.as_deref().unwrap_or(&r.target), r.bytes.as_ref());
        }
        let mut hash = blake3::Hasher::new();
        hash.update(b"feritto-capture-policy-v1\0");
        for (name, bytes) in formats {
            hash.update(&(name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        hash.finalize().to_hex().to_string()
    }
}

pub fn is_text(target: &str) -> bool {
    matches!(
        target,
        "UTF8_STRING"
            | "STRING"
            | "TEXT"
            | "text/plain"
            | "text/plain;charset=utf-8"
            | "text/plain;charset=UTF-8"
    )
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn bounded_text(text: &str, max_bytes: usize) -> String {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// NFKC + Unicode default case folding. Search and persistence use the same predicate.
pub fn normalize_search(text: &str) -> String {
    use unicode_casefold::UnicodeCaseFold;
    text.nfkc().case_fold().nfc().collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipSummary {
    pub id: ClipId,
    pub preview: String,
    pub formats: Vec<String>,
    pub captured_at: i64,
    pub size: u64,
    pub starred: bool,
    pub search_truncated: bool,
    pub has_image: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub application: Option<String>,
    pub window: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Capture {
    pub generation: u64,
    pub offer: Arc<Offer>,
    pub source: SourceIdentity,
    pub complete: bool,
    pub omitted: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Capability {
    Available,
    PermissionRequired(String),
    Unavailable(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub backend: String,
    pub monitor: Capability,
    pub ownership: Capability,
    pub global_hotkey: Capability,
    pub automatic_paste: Capability,
    pub source_identity: Capability,
    pub pointer_placement: Capability,
    pub caret_placement: Capability,
    pub persistence: Capability,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PasteTarget {
    pub window: u32,
    pub timestamp: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum PasteShortcut {
    #[default]
    ControlV,
    ControlShiftV,
    ShiftInsert,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CapturePolicy {
    pub text: bool,
    pub images: bool,
    pub html: bool,
    pub rtf: bool,
    pub files: bool,
    pub extra_targets: Vec<String>,
    pub excluded_applications: Vec<String>,
    pub max_target_bytes: usize,
    pub max_generation_bytes: usize,
    pub progress_timeout_ms: u64,
    pub generation_timeout_ms: u64,
}
impl Default for CapturePolicy {
    fn default() -> Self {
        Self {
            text: true,
            images: true,
            html: true,
            rtf: true,
            files: true,
            extra_targets: vec![],
            excluded_applications: vec![],
            max_target_bytes: 32 * 1024 * 1024,
            max_generation_bytes: 64 * 1024 * 1024,
            progress_timeout_ms: 2000,
            generation_timeout_ms: 10000,
        }
    }
}
impl CapturePolicy {
    pub fn select_targets(&self, offered: &[String]) -> Vec<String> {
        let mut selected = Vec::new();
        let mut prefer = |names: &[&str]| {
            if let Some(name) = names.iter().find(|n| offered.iter().any(|o| o == **n)) {
                selected.push((*name).to_string());
            }
        };
        if self.text {
            prefer(&[
                "UTF8_STRING",
                "text/plain;charset=utf-8",
                "text/plain;charset=UTF-8",
                "text/plain",
                "STRING",
            ]);
        }
        if self.images {
            prefer(&["image/png"]);
        }
        if self.html {
            prefer(&["text/html"]);
        }
        if self.rtf {
            prefer(&["text/rtf", "application/rtf"]);
        }
        if self.files {
            prefer(&["text/uri-list"]);
            prefer(&["x-special/gnome-copied-files"]);
        }
        for extra in &self.extra_targets {
            if !is_protocol_target(extra) && offered.contains(extra) && !selected.contains(extra) {
                selected.push(extra.clone());
            }
        }
        selected
    }
}
pub fn is_protocol_target(target: &str) -> bool {
    matches!(
        target,
        "TARGETS"
            | "TIMESTAMP"
            | "MULTIPLE"
            | "SAVE_TARGETS"
            | "INCR"
            | "DELETE"
            | "INSERT_PROPERTY"
            | "INSERT_SELECTION"
            | "ATOM"
            | "ATOM_PAIR"
            | "x-kde-passwordManagerHint"
            | "application/x-feritto-ignore"
            | "Clipboard Viewer Ignore"
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub capture: CapturePolicy,
    pub max_clips: usize,
    pub max_age_days: u32,
    pub max_history_bytes: u64,
    pub automatic_paste: bool,
    pub paste_shortcut: PasteShortcut,
    pub paused: bool,
    pub coexistence: bool,
    pub dark_theme: bool,
    pub clear_clipboard_on_exit: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            capture: CapturePolicy::default(),
            max_clips: 10000,
            max_age_days: 30,
            max_history_bytes: 512 * 1024 * 1024,
            automatic_paste: true,
            paste_shortcut: PasteShortcut::ControlV,
            paused: false,
            coexistence: false,
            dark_theme: true,
            clear_clipboard_on_exit: false,
        }
    }
}

pub type ServiceResult<T> = Result<T, String>;
pub trait ClipboardMonitor {
    fn set_paused(&self, paused: bool) -> ServiceResult<()>;
}
pub trait ClipboardOwner {
    fn publish(&self, offer: Arc<Offer>) -> ServiceResult<()>;
    fn clear(&self) -> ServiceResult<()>;
}
pub trait HotkeyService {
    fn hotkey_capability(&self) -> Capability;
}
pub trait PasteInjector {
    fn paste(
        &self,
        offer: Arc<Offer>,
        target: PasteTarget,
        shortcut: PasteShortcut,
    ) -> ServiceResult<()>;
}
pub trait ActiveWindowTracker {
    fn active_window(&self) -> ServiceResult<PasteTarget>;
}
pub trait PopupPlacer {
    fn prepare_popup(&self, window: u32, width: u32, height: u32) -> ServiceResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rich_formats_affect_dedup() {
        let a = Offer::text("same");
        let mut b = a.clone();
        b.representations.push(Representation {
            target: "text/html".into(),
            mime: Some("text/html".into()),
            bytes: Arc::from(b"<b>same</b>".as_slice()),
        });
        assert_ne!(a.digest(), b.digest());
        b.representations.reverse();
        let mut c = b.clone();
        c.representations.reverse();
        assert_eq!(b.digest(), c.digest());
    }
    #[test]
    fn policy_resolves_aliases_and_ignores_protocol() {
        let policy = CapturePolicy {
            extra_targets: vec!["INCR".into(), "application/test".into()],
            ..Default::default()
        };
        let offered = [
            "STRING",
            "UTF8_STRING",
            "image/png",
            "image/jpeg",
            "TARGETS",
            "INCR",
            "application/test",
        ]
        .map(String::from);
        assert_eq!(
            policy.select_targets(&offered),
            vec!["UTF8_STRING", "image/png", "application/test"]
        );
    }
    #[test]
    fn unicode_search_and_boundary() {
        assert_eq!(
            normalize_search("Ｓtraße CAFÉ"),
            normalize_search("strasse cafe\u{301}")
        );
        assert_eq!(bounded_text("a😀b", 4), "a");
    }
}
