//! xdg-desktop-portal integration for Wayland: the GlobalShortcuts portal registers the popup
//! hotkey, and the RemoteDesktop portal provides permission-backed keyboard access for
//! automatic paste on compositors without the virtual-keyboard protocol.
//!
//! Portal calls can wait for user dialogs, so they run on their own thread with a
//! current-thread Tokio runtime. Results return to the Wayland worker as `PortalUpdate`s.
//! Every call that can show a dialog runs as its own task: a dialog left open never stalls
//! shortcut activations, injections, or another dialog.
use anyhow::{Context, Result, bail};
use ashpd::desktop::{
    PersistMode, Session,
    global_shortcuts::{GlobalShortcuts, NewShortcut},
    remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
};
use feritto_core::Capability;
use futures_util::{Stream, StreamExt};
use std::{
    path::PathBuf,
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Instant,
};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::{LocalSet, spawn_local},
};

pub const APP_ID: &str = "io.github.gh2302.Feritto";
const TOGGLE: &str = "toggle-history";
/// The final status of a paste whose clipboard contents were replaced before keys were sent.
pub const PASTE_SUPERSEDED: &str = "Clipboard changed before paste; no keys were sent";

/// Counts every change of the contents feritto serves. The Wayland worker advances them; the
/// portal thread reads them to reject injections queued for contents that are gone.
#[derive(Debug, Default)]
pub struct Publications {
    clipboard: AtomicU64,
    primary: AtomicU64,
}
impl Publications {
    pub fn advance(&self, clipboard: bool, primary: bool) {
        if clipboard {
            self.clipboard.fetch_add(1, Ordering::SeqCst);
        }
        if primary {
            self.primary.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Binds a paste to the publication it was requested for. Keys may be sent only while the
/// guard holds: the CLIPBOARD contents (and PRIMARY, for Shift+Insert) are still the ones the
/// user chose, and an optional deadline for queued work has not passed.
#[derive(Debug, Clone)]
pub struct PasteGuard {
    shared: Arc<Publications>,
    clipboard: u64,
    primary: Option<u64>,
    deadline: Option<Instant>,
}
impl PasteGuard {
    pub fn new(shared: &Arc<Publications>, primary: bool) -> Self {
        Self {
            shared: shared.clone(),
            clipboard: shared.clipboard.load(Ordering::SeqCst),
            primary: primary.then(|| shared.primary.load(Ordering::SeqCst)),
            deadline: None,
        }
    }
    pub fn until(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }
    pub fn check(&self) -> Result<(), String> {
        let primary_changed = self
            .primary
            .is_some_and(|p| p != self.shared.primary.load(Ordering::SeqCst));
        if self.clipboard != self.shared.clipboard.load(Ordering::SeqCst) || primary_changed {
            return Err(PASTE_SUPERSEDED.into());
        }
        if self.deadline.is_some_and(|d| Instant::now() > d) {
            return Err(
                "Copied; press your paste shortcut (keyboard access did not respond in time)"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum PortalRequest {
    /// Start a RemoteDesktop session, showing the portal's permission dialog if needed.
    GrantPaste,
    /// Press `modifiers` then `key` (X keysyms), and release them in reverse order, if the
    /// guard still holds when the keys are about to be sent.
    Inject {
        modifiers: Vec<u32>,
        key: u32,
        guard: PasteGuard,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteAccess {
    Unavailable(String),
    PermissionRequired(String),
    /// A portal request (possibly a dialog) is in progress.
    Pending(String),
    Ready,
}

#[derive(Debug)]
pub enum InjectError {
    /// No keys were sent because the paste no longer applies; this is the final status.
    Stale(String),
    /// Keyboard access failed.
    Failed(String),
}

#[derive(Debug)]
pub enum PortalUpdate {
    Hotkey(Capability),
    Paste(PasteAccess),
    /// The global shortcut fired; the portal may supply an xdg-activation token.
    Activated(Option<String>),
    Injected(Result<(), InjectError>),
    /// How a user-requested keyboard-control dialog ended.
    Status(String),
}

pub struct PortalOptions {
    pub token_path: PathBuf,
    /// Whether to manage RemoteDesktop keyboard access at all. It is skipped when the
    /// compositor already offers a virtual keyboard that needs no permission.
    pub remote_desktop: bool,
    /// Whether to register the hotkey through the GlobalShortcuts portal.
    pub shortcuts: bool,
}

type Updates = Rc<dyn Fn(PortalUpdate)>;

pub fn start(
    options: PortalOptions,
    updates: impl Fn(PortalUpdate) + Send + 'static,
) -> Result<UnboundedSender<PortalRequest>> {
    let (tx, rx) = unbounded_channel();
    thread::Builder::new()
        .name("feritto-portal".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let reason = format!("portal runtime failed: {error}");
                    updates(PortalUpdate::Hotkey(Capability::Unavailable(
                        reason.clone(),
                    )));
                    updates(PortalUpdate::Paste(PasteAccess::Unavailable(reason)));
                    return;
                }
            };
            // Tasks spawned on the set end with it, when the worker drops its sender.
            LocalSet::new().block_on(&runtime, run(options, rx, Rc::new(updates)));
        })?;
    Ok(tx)
}

type Activations = Pin<Box<dyn Stream<Item = ashpd::desktop::global_shortcuts::Activated> + Send>>;

struct Remote {
    proxy: RemoteDesktop,
    session: Session<RemoteDesktop>,
}
impl Remote {
    async fn close(self) {
        if let Err(error) = self.session.close().await {
            tracing::debug!(%error, "could not close the keyboard-control session");
        }
    }
}

/// The result of a task that obtains keyboard access.
enum Grant {
    /// Startup: a stored grant was restored, or none exists.
    Restored(Result<Option<Remote>>),
    /// The user answered the permission dialog.
    Requested(Result<Remote>),
}

async fn run(options: PortalOptions, mut rx: UnboundedReceiver<PortalRequest>, updates: Updates) {
    // Host (unsandboxed) applications must name themselves before other portal calls so the
    // portal can attribute shortcuts and permissions to the desktop entry.
    if let Ok(app_id) = ashpd::AppID::try_from(APP_ID)
        && let Err(error) = ashpd::register_host_app(app_id).await
    {
        tracing::debug!(%error, "portal host registry unavailable");
    }
    if options.shortcuts {
        spawn_local(shortcuts(updates.clone()));
    }
    let (grants, mut granted) = unbounded_channel::<Grant>();
    let mut remote: Option<Remote> = None;
    let mut granting = false;
    if options.remote_desktop {
        granting = true;
        updates(PortalUpdate::Paste(PasteAccess::Pending(
            "checking for saved keyboard-control access".into(),
        )));
        let (path, grants) = (options.token_path.clone(), grants.clone());
        spawn_local(async move {
            let _ = grants.send(Grant::Restored(restore_or_probe(&path).await));
        });
    }
    loop {
        tokio::select! {
            request = rx.recv() => match request {
                None => break,
                Some(PortalRequest::GrantPaste) => {
                    if !options.remote_desktop || granting {
                        continue;
                    }
                    granting = true;
                    updates(PortalUpdate::Paste(PasteAccess::Pending(
                        "waiting for you to allow keyboard control in the desktop dialog".into(),
                    )));
                    let (path, grants) = (options.token_path.clone(), grants.clone());
                    spawn_local(async move {
                        let _ = grants.send(Grant::Requested(start_remote(&path, None).await));
                    });
                }
                Some(PortalRequest::Inject { modifiers, key, guard }) => {
                    let result = match &remote {
                        Some(session) => inject(session, &modifiers, key, &guard).await,
                        None if granting => Err(InjectError::Failed(
                            "keyboard control is still being requested".into(),
                        )),
                        None => Err(InjectError::Failed(
                            "keyboard control has not been granted".into(),
                        )),
                    };
                    if matches!(result, Err(InjectError::Failed(_)))
                        && let Some(session) = remote.take()
                    {
                        // A revoked or broken session needs a new grant; end it explicitly.
                        session.close().await;
                        updates(PortalUpdate::Paste(PasteAccess::PermissionRequired(
                            "keyboard control ended; allow it again in Settings".into(),
                        )));
                    }
                    updates(PortalUpdate::Injected(result));
                }
            },
            Some(grant) = granted.recv() => {
                granting = false;
                match &grant {
                    Grant::Requested(Ok(_)) => updates(PortalUpdate::Status(
                        "Keyboard control for automatic paste is allowed".into(),
                    )),
                    Grant::Requested(Err(error)) => updates(PortalUpdate::Status(format!(
                        "Keyboard control was not granted: {error:#}"
                    ))),
                    Grant::Restored(_) => {}
                }
                let (session, access) = match grant {
                    Grant::Restored(Ok(Some(session))) | Grant::Requested(Ok(session)) => {
                        (Some(session), PasteAccess::Ready)
                    }
                    Grant::Restored(Ok(None)) => (None, PasteAccess::PermissionRequired(
                        "allow keyboard control for automatic paste in Settings".into(),
                    )),
                    Grant::Restored(Err(error)) => {
                        (None, PasteAccess::Unavailable(format!("{error:#}")))
                    }
                    Grant::Requested(Err(error)) => (None, PasteAccess::PermissionRequired(
                        format!("keyboard control was not granted ({error:#})"),
                    )),
                };
                match session {
                    Some(session) => {
                        if let Some(old) = remote.replace(session) {
                            old.close().await;
                        }
                        updates(PortalUpdate::Paste(access));
                    }
                    // A declined request leaves an earlier working session in place.
                    None if remote.is_some() => updates(PortalUpdate::Paste(PasteAccess::Ready)),
                    None => updates(PortalUpdate::Paste(access)),
                }
            }
        }
    }
    if let Some(session) = remote {
        session.close().await;
    }
}

/// Binds the popup shortcut and forwards its activations until the desktop ends the session.
async fn shortcuts(updates: Updates) {
    updates(PortalUpdate::Hotkey(Capability::Pending(
        "waiting for the desktop to confirm the Ctrl+` shortcut".into(),
    )));
    let (_session, mut activations) = match bind_shortcut().await {
        Ok(bound) => bound,
        Err(error) => {
            tracing::info!(%error, "global shortcut portal unavailable");
            updates(PortalUpdate::Hotkey(Capability::Unavailable(format!(
                "{error:#}; bind `feritto --toggle` to a key in your desktop settings"
            ))));
            return;
        }
    };
    updates(PortalUpdate::Hotkey(Capability::Available));
    // The session object keeps the shortcut bound for as long as it lives.
    while let Some(activated) = activations.next().await {
        if activated.shortcut_id() == TOGGLE {
            let token = activated
                .options()
                .get("activation_token")
                .and_then(|value| String::try_from(value.clone()).ok())
                .filter(|token| !token.is_empty());
            updates(PortalUpdate::Activated(token));
        }
    }
    updates(PortalUpdate::Hotkey(Capability::Unavailable(
        "the desktop ended the shortcut session; restart feritto or bind `feritto --toggle` to a key".into(),
    )));
}

async fn bind_shortcut() -> Result<(Session<GlobalShortcuts>, Activations)> {
    let portal = GlobalShortcuts::new()
        .await
        .context("no GlobalShortcuts portal")?;
    let session = portal
        .create_session(Default::default())
        .await
        .context("could not create a shortcut session")?;
    let shortcut =
        NewShortcut::new(TOGGLE, "Show clipboard history").preferred_trigger("CTRL+grave");
    portal
        .bind_shortcuts(&session, &[shortcut], None, Default::default())
        .await?
        .response()
        .context("the desktop did not bind the shortcut")?;
    let stream = portal.receive_activated().await?;
    Ok((session, Box::pin(stream)))
}

/// Restores a previously granted session silently, or reports whether a grant is possible.
async fn restore_or_probe(token_path: &PathBuf) -> Result<Option<Remote>> {
    let proxy = RemoteDesktop::new()
        .await
        .context("no RemoteDesktop portal")?;
    let devices = proxy
        .available_device_types()
        .await
        .context("RemoteDesktop portal is unusable")?;
    if !devices.contains(DeviceType::Keyboard) {
        bail!("the RemoteDesktop portal offers no keyboard device");
    }
    let Ok(token) = std::fs::read_to_string(token_path) else {
        return Ok(None);
    };
    let token = token.trim().to_owned();
    if token.is_empty() || proxy.version() < 2 {
        return Ok(None);
    }
    match start_remote(token_path, Some(token)).await {
        Ok(session) => Ok(Some(session)),
        Err(error) => {
            tracing::info!(%error, "stored keyboard-control grant was not restored");
            Ok(None)
        }
    }
}

async fn start_remote(token_path: &PathBuf, token: Option<String>) -> Result<Remote> {
    let proxy = RemoteDesktop::new()
        .await
        .context("no RemoteDesktop portal")?;
    let session = proxy.create_session(Default::default()).await?;
    let remote = Remote { proxy, session };
    match select_and_start(&remote, token_path, token).await {
        Ok(()) => Ok(remote),
        Err(error) => {
            // A session that was created but not started must not linger in the portal.
            remote.close().await;
            Err(error)
        }
    }
}

async fn select_and_start(
    remote: &Remote,
    token_path: &PathBuf,
    token: Option<String>,
) -> Result<()> {
    let Remote { proxy, session } = remote;
    let mut select = SelectDevicesOptions::default()
        .set_devices(ashpd::enumflags2::BitFlags::from(DeviceType::Keyboard));
    if proxy.version() >= 2 {
        select = select
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(token.as_deref());
    }
    proxy.select_devices(session, select).await?.response()?;
    let selected = proxy
        .start(session, None, Default::default())
        .await?
        .response()
        .context("keyboard control was declined")?;
    if !selected.devices().contains(DeviceType::Keyboard) {
        bail!("keyboard control was not included in the grant");
    }
    if let Some(token) = selected.restore_token() {
        save_token(token_path, token);
    }
    Ok(())
}

fn save_token(path: &PathBuf, token: &str) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let result = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(token.as_bytes()));
    if let Err(error) = result {
        tracing::warn!(%error, "could not store the keyboard-control grant");
    }
}

/// Sends the shortcut. Every key whose press was attempted is released in reverse order even
/// when an earlier call failed, so no modifier can stay held on the user's keyboard.
async fn inject(
    remote: &Remote,
    modifiers: &[u32],
    key: u32,
    guard: &PasteGuard,
) -> Result<(), InjectError> {
    // The last moment to back out: once the first key is down, the shortcut is completed.
    guard.check().map_err(InjectError::Stale)?;
    let send = |keysym: u32, state: KeyState| {
        remote.proxy.notify_keyboard_keysym(
            &remote.session,
            keysym as i32,
            state,
            Default::default(),
        )
    };
    let mut attempted = Vec::with_capacity(modifiers.len() + 1);
    let mut result = Ok(());
    for &keysym in modifiers.iter().chain([&key]) {
        // A failed call may still have reached the compositor, so its key is released too.
        attempted.push(keysym);
        if let Err(error) = send(keysym, KeyState::Pressed).await {
            result = Err(error);
            break;
        }
    }
    for &keysym in attempted.iter().rev() {
        if let Err(error) = send(keysym, KeyState::Released).await {
            tracing::warn!(%error, keysym, "could not release an injected key");
            if result.is_ok() {
                result = Err(error);
            }
        }
    }
    result.map_err(|error| InjectError::Failed(format!("{error:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn paste_guard_follows_the_publication_it_was_made_for() {
        let shared = Arc::new(Publications::default());
        let clipboard = PasteGuard::new(&shared, false);
        let primary = PasteGuard::new(&shared, true);
        assert!(clipboard.check().is_ok() && primary.check().is_ok());
        // Only Shift+Insert depends on PRIMARY.
        shared.advance(false, true);
        assert!(clipboard.check().is_ok());
        assert_eq!(primary.check().unwrap_err(), PASTE_SUPERSEDED);
        // A replaced CLIPBOARD voids every earlier paste.
        shared.advance(true, false);
        assert_eq!(clipboard.check().unwrap_err(), PASTE_SUPERSEDED);
        let fresh = PasteGuard::new(&shared, false);
        assert!(fresh.check().is_ok());
        let expired = fresh.until(Instant::now() - Duration::from_millis(1));
        assert!(expired.check().unwrap_err().starts_with("Copied;"));
    }
}
