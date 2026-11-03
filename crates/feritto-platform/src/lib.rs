//! Native adapters live here. Wayland will implement the same core contracts.
mod x11;
use anyhow::{Result, bail};
pub use x11::{PlatformEvent, X11Service};

pub fn require_x11_session() -> Result<()> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|s| !s.is_empty())
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s.eq_ignore_ascii_case("wayland"));
    if wayland {
        bail!(
            "Wayland support is pending. Start feritto in a native X11 session; Xwayland cannot monitor or paste into native Wayland applications."
        );
    }
    if std::env::var_os("DISPLAY").is_none_or(|s| s.is_empty()) {
        bail!("No X11 DISPLAY is set. Start feritto from your X11 desktop session.");
    }
    Ok(())
}
