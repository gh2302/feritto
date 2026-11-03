//! Native adapters. X11 and Wayland implement the same core contracts; `Platform` selects
//! one at runtime from the session and hides the difference from the executable and UI.
mod portal;
mod wayland;
mod x11;
use anyhow::{Result, bail};
use crossbeam_channel::Receiver;
use feritto_core::*;
pub use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::{path::PathBuf, sync::Arc};
pub use wayland::{WaylandService, activate_surface};
pub use x11::{PlatformEvent, X11Service};

/// The display protocol that the desktop session runs natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    X11,
    Wayland,
}

/// Process-level inputs that are not user settings.
#[derive(Debug, Clone, Default)]
pub struct PlatformOptions {
    /// Private directory for platform state such as portal restore tokens.
    pub state_dir: PathBuf,
}

fn non_empty(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|s| !s.is_empty())
}

/// Chooses the native backend. A Wayland session always uses the Wayland adapter, even when
/// Xwayland also sets DISPLAY: Xwayland cannot observe or paste into native Wayland clients.
pub fn detect_session() -> Result<Backend> {
    if non_empty("WAYLAND_DISPLAY") {
        return Ok(Backend::Wayland);
    }
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s.eq_ignore_ascii_case("wayland")) {
        bail!(
            "This is a Wayland session, but WAYLAND_DISPLAY is not set. Start feritto from the desktop session."
        );
    }
    if non_empty("DISPLAY") {
        return Ok(Backend::X11);
    }
    bail!("Neither WAYLAND_DISPLAY nor DISPLAY is set. Start feritto from your desktop session.")
}

/// Used by the X11 adapter, which must never run against Xwayland inside a Wayland session.
pub fn require_x11_session() -> Result<()> {
    if detect_session()? != Backend::X11 {
        bail!(
            "The X11 adapter cannot serve a Wayland session; Xwayland cannot monitor or paste into native Wayland applications."
        );
    }
    Ok(())
}

#[derive(Clone)]
pub enum Platform {
    X11(X11Service),
    Wayland(WaylandService),
}

impl Platform {
    pub fn start(
        settings: Settings,
        options: PlatformOptions,
    ) -> Result<(Self, Receiver<PlatformEvent>)> {
        Ok(match detect_session()? {
            Backend::X11 => {
                let (service, events) = X11Service::start(settings)?;
                (Self::X11(service), events)
            }
            Backend::Wayland => {
                let (service, events) = WaylandService::start(settings, options)?;
                (Self::Wayland(service), events)
            }
        })
    }
    pub fn backend(&self) -> Backend {
        match self {
            Self::X11(_) => Backend::X11,
            Self::Wayland(_) => Backend::Wayland,
        }
    }
    pub fn capabilities(&self) -> Capabilities {
        match self {
            Self::X11(s) => s.capabilities(),
            Self::Wayland(s) => s.capabilities(),
        }
    }
    pub fn configure(&self, settings: Settings) {
        match self {
            Self::X11(s) => s.configure(settings),
            Self::Wayland(s) => s.configure(settings),
        }
    }
    /// X11 only: the native popup window was mapped and should receive focus.
    pub fn focus_popup(&self, window: u32) {
        if let Self::X11(s) = self {
            s.focus_popup(window);
        }
    }
    pub fn restore(&self, target: PasteTarget) {
        match self {
            Self::X11(s) => s.restore(target),
            Self::Wayland(s) => s.restore(target),
        }
    }
    pub fn shutdown(&self) {
        match self {
            Self::X11(s) => s.shutdown(),
            Self::Wayland(s) => s.shutdown(),
        }
    }
    /// Wayland only: an xdg-activation token from the launcher, CLI, or shortcut portal that
    /// lets the next popup take keyboard focus.
    pub fn set_activation_token(&self, token: Option<String>) {
        if let Self::Wayland(s) = self {
            s.set_activation_token(token);
        }
    }
    /// Wayland only: activates a freshly opened window with the pending token, if any.
    /// Must run on the thread that owns the window (inside iced's `window::run`).
    pub fn activate_window(
        &self,
        display: RawDisplayHandle,
        window: RawWindowHandle,
    ) -> ServiceResult<bool> {
        match self {
            Self::X11(_) => Ok(false),
            Self::Wayland(s) => s.activate_window(display, window),
        }
    }
    /// Wayland only: current clipboard text for pasting into feritto's own search field.
    pub fn clipboard_text(&self) -> ServiceResult<String> {
        match self {
            Self::X11(_) => Err("The X11 GUI reads the clipboard directly".into()),
            Self::Wayland(s) => s.clipboard_text(),
        }
    }
    /// Wayland only: asks the RemoteDesktop portal for keyboard access used by automatic paste.
    pub fn request_paste_permission(&self) -> ServiceResult<()> {
        match self {
            Self::X11(_) => Err("Automatic paste does not need a permission on X11".into()),
            Self::Wayland(s) => s.request_paste_permission(),
        }
    }
}

impl ClipboardMonitor for Platform {
    fn set_paused(&self, paused: bool) -> ServiceResult<()> {
        match self {
            Self::X11(s) => s.set_paused(paused),
            Self::Wayland(s) => s.set_paused(paused),
        }
    }
}
impl ClipboardOwner for Platform {
    fn publish(&self, offer: Arc<Offer>) -> ServiceResult<()> {
        match self {
            Self::X11(s) => s.publish(offer),
            Self::Wayland(s) => s.publish(offer),
        }
    }
    fn clear(&self) -> ServiceResult<()> {
        match self {
            Self::X11(s) => s.clear(),
            Self::Wayland(s) => s.clear(),
        }
    }
}
impl HotkeyService for Platform {
    fn hotkey_capability(&self) -> Capability {
        self.capabilities().global_hotkey
    }
}
impl PasteInjector for Platform {
    fn paste(
        &self,
        offer: Arc<Offer>,
        target: PasteTarget,
        shortcut: PasteShortcut,
    ) -> ServiceResult<()> {
        match self {
            Self::X11(s) => s.paste(offer, target, shortcut),
            Self::Wayland(s) => s.paste(offer, target, shortcut),
        }
    }
}
impl ActiveWindowTracker for Platform {
    fn active_window(&self) -> ServiceResult<PasteTarget> {
        match self {
            Self::X11(s) => s.active_window(),
            Self::Wayland(s) => s.active_window(),
        }
    }
}
impl PopupPlacer for Platform {
    fn prepare_popup(&self, window: u32, width: u32, height: u32) -> ServiceResult<()> {
        match self {
            Self::X11(s) => s.prepare_popup(window, width, height),
            // The compositor places Wayland windows; the popup is created when it is shown.
            Self::Wayland(_) => Ok(()),
        }
    }
}
