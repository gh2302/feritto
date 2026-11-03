//! xdg-desktop-portal integration for Wayland: the GlobalShortcuts portal registers the popup
//! hotkey, and the RemoteDesktop portal provides permission-backed keyboard access for
//! automatic paste on compositors without the virtual-keyboard protocol.
//!
//! Portal calls can wait for user dialogs, so they run on their own thread with a
//! current-thread Tokio runtime. Results return to the Wayland worker as `PortalUpdate`s.
use anyhow::{Context, Result, bail};
use ashpd::desktop::{
    PersistMode, Session,
    global_shortcuts::{GlobalShortcuts, NewShortcut},
    remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
};
use feritto_core::Capability;
use futures_util::{Stream, StreamExt};
use std::{path::PathBuf, pin::Pin, thread};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub const APP_ID: &str = "io.github.gh2302.Feritto";
const TOGGLE: &str = "toggle-history";

#[derive(Debug)]
pub enum PortalRequest {
    /// Start a RemoteDesktop session, showing the portal's permission dialog if needed.
    GrantPaste,
    /// Press `modifiers` then `key` (X keysyms), and release them in reverse order.
    Inject { modifiers: Vec<u32>, key: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteAccess {
    Unavailable(String),
    PermissionRequired(String),
    Ready,
}

#[derive(Debug)]
pub enum PortalUpdate {
    Hotkey(Capability),
    Paste(PasteAccess),
    /// The global shortcut fired; the portal may supply an xdg-activation token.
    Activated(Option<String>),
    Injected(Result<(), String>),
}

pub struct PortalOptions {
    pub token_path: PathBuf,
    /// Whether to manage RemoteDesktop keyboard access at all. It is skipped when the
    /// compositor already offers a virtual keyboard that needs no permission.
    pub remote_desktop: bool,
    /// Whether to register the hotkey through the GlobalShortcuts portal.
    pub shortcuts: bool,
}

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
            runtime.block_on(run(options, rx, &updates));
        })?;
    Ok(tx)
}

type Activations = Pin<Box<dyn Stream<Item = ashpd::desktop::global_shortcuts::Activated> + Send>>;

struct Remote {
    proxy: RemoteDesktop,
    session: Session<RemoteDesktop>,
}

async fn run(
    options: PortalOptions,
    mut rx: UnboundedReceiver<PortalRequest>,
    updates: &(impl Fn(PortalUpdate) + Send),
) {
    // Host (unsandboxed) applications must name themselves before other portal calls so the
    // portal can attribute shortcuts and permissions to the desktop entry.
    if let Ok(app_id) = ashpd::AppID::try_from(APP_ID)
        && let Err(error) = ashpd::register_host_app(app_id).await
    {
        tracing::debug!(%error, "portal host registry unavailable");
    }
    let mut activations: Option<Activations> = None;
    // Keep the session object alive for as long as the shortcut should stay bound.
    let mut _shortcut_session = None;
    if options.shortcuts {
        match bind_shortcut().await {
            Ok((session, stream)) => {
                _shortcut_session = Some(session);
                activations = Some(stream);
                updates(PortalUpdate::Hotkey(Capability::Available));
            }
            Err(error) => {
                tracing::info!(%error, "global shortcut portal unavailable");
                updates(PortalUpdate::Hotkey(Capability::Unavailable(format!(
                    "{error:#}; bind `feritto --toggle` to a key in your desktop settings"
                ))));
            }
        }
    }
    let mut remote: Option<Remote> = None;
    if options.remote_desktop {
        let access = match restore_or_probe(&options.token_path).await {
            Ok(Some(session)) => {
                remote = Some(session);
                PasteAccess::Ready
            }
            Ok(None) => PasteAccess::PermissionRequired(
                "allow keyboard control for automatic paste in Settings".into(),
            ),
            Err(error) => PasteAccess::Unavailable(format!("{error:#}")),
        };
        updates(PortalUpdate::Paste(access));
    }
    loop {
        let activation = async {
            match activations.as_mut() {
                Some(stream) => stream.next().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            request = rx.recv() => match request {
                None => break,
                Some(PortalRequest::GrantPaste) => {
                    if !options.remote_desktop {
                        continue;
                    }
                    match start_remote(&options.token_path, None).await {
                        Ok(session) => {
                            remote = Some(session);
                            updates(PortalUpdate::Paste(PasteAccess::Ready));
                        }
                        Err(error) => updates(PortalUpdate::Paste(PasteAccess::PermissionRequired(
                            format!("keyboard control was not granted ({error:#})"),
                        ))),
                    }
                }
                Some(PortalRequest::Inject { modifiers, key }) => {
                    let result = match &remote {
                        Some(remote) => inject(remote, &modifiers, key).await,
                        None => Err(anyhow::anyhow!("keyboard control has not been granted")),
                    };
                    if result.is_err() {
                        // A revoked or closed session needs a new grant.
                        remote = None;
                        updates(PortalUpdate::Paste(PasteAccess::PermissionRequired(
                            "keyboard control ended; allow it again in Settings".into(),
                        )));
                    }
                    updates(PortalUpdate::Injected(result.map_err(|e| format!("{e:#}"))));
                }
            },
            Some(activated) = activation => {
                if activated.shortcut_id() == TOGGLE {
                    let token = activated
                        .options()
                        .get("activation_token")
                        .and_then(|value| String::try_from(value.clone()).ok())
                        .filter(|token| !token.is_empty());
                    updates(PortalUpdate::Activated(token));
                }
            }
        }
    }
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
    let mut select = SelectDevicesOptions::default()
        .set_devices(ashpd::enumflags2::BitFlags::from(DeviceType::Keyboard));
    if proxy.version() >= 2 {
        select = select
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(token.as_deref());
    }
    proxy.select_devices(&session, select).await?.response()?;
    let selected = proxy
        .start(&session, None, Default::default())
        .await?
        .response()
        .context("keyboard control was declined")?;
    if !selected.devices().contains(DeviceType::Keyboard) {
        bail!("keyboard control was not included in the grant");
    }
    if let Some(token) = selected.restore_token() {
        save_token(token_path, token);
    }
    Ok(Remote { proxy, session })
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

async fn inject(remote: &Remote, modifiers: &[u32], key: u32) -> Result<()> {
    let send = |keysym: u32, state: KeyState| {
        remote.proxy.notify_keyboard_keysym(
            &remote.session,
            keysym as i32,
            state,
            Default::default(),
        )
    };
    for &modifier in modifiers {
        send(modifier, KeyState::Pressed).await?;
    }
    let pressed = send(key, KeyState::Pressed).await;
    // Always release what was pressed, even when a later call fails.
    let released = send(key, KeyState::Released).await;
    for &modifier in modifiers.iter().rev() {
        send(modifier, KeyState::Released).await?;
    }
    pressed?;
    released?;
    Ok(())
}
