//! Native Wayland adapter.
//!
//! - Clipboard history uses ext-data-control-v1, or wlr-data-control-unstable-v1 as a fallback.
//!   Both let a background client observe, read, and own the selection without focus.
//! - Target tracking, focus restoration, and best-effort source identity use
//!   wlr-foreign-toplevel-management where the compositor offers it.
//! - Automatic paste uses zwp-virtual-keyboard-v1, or keyboard access granted through the
//!   RemoteDesktop portal. Keys are sent only after the target window is confirmed active,
//!   unless the user explicitly allows unverified paste on compositors that cannot confirm it.
//! - The GlobalShortcuts portal registers the popup hotkey; `feritto --toggle` is the fallback.
//!
//! Every missing protocol or portal is reported as a capability with its reason. Nothing falls
//! back to Xwayland, which cannot observe or paste into native Wayland clients.
use crate::{
    PlatformEvent, PlatformOptions, RawDisplayHandle, RawWindowHandle,
    portal::{self, APP_ID, PasteAccess, PortalOptions, PortalRequest, PortalUpdate},
};
use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, bounded};
use feritto_core::*;
use rustix::{event::PollFlags, io::Errno};
use std::{
    collections::{HashMap, VecDeque},
    io::Write,
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::UnixDatagram,
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::{ObjectId, WaylandError},
    event_created_child,
    globals::{GlobalList, GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat::WlSeat, wl_surface::WlSurface},
};
use wayland_protocols::{
    ext::data_control::v1::client::{
        ext_data_control_device_v1::{self as ext_device, ExtDataControlDeviceV1},
        ext_data_control_manager_v1::ExtDataControlManagerV1,
        ext_data_control_offer_v1::{self as ext_offer, ExtDataControlOfferV1},
        ext_data_control_source_v1::{self as ext_source, ExtDataControlSourceV1},
    },
    xdg::activation::v1::client::{
        xdg_activation_token_v1::XdgActivationTokenV1, xdg_activation_v1::XdgActivationV1,
    },
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::{
    data_control::v1::client::{
        zwlr_data_control_device_v1::{self as wlr_device, ZwlrDataControlDeviceV1},
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
        zwlr_data_control_offer_v1::{self as wlr_offer, ZwlrDataControlOfferV1},
        zwlr_data_control_source_v1::{self as wlr_source, ZwlrDataControlSourceV1},
    },
    foreign_toplevel::v1::client::{
        zwlr_foreign_toplevel_handle_v1::{self as toplevel_handle, ZwlrForeignToplevelHandleV1},
        zwlr_foreign_toplevel_manager_v1::{
            self as toplevel_manager, ZwlrForeignToplevelManagerV1,
        },
    },
};

const SECRET_HINT: &str = "x-kde-passwordManagerHint";
const IGNORE_HINTS: [&str; 2] = ["application/x-feritto-ignore", "Clipboard Viewer Ignore"];
/// Text names served for any captured text. Wayland toolkits use the MIME names; Xwayland
/// clients and older toolkits still request the X11 names through the compositor bridge.
const TEXT_TARGETS: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain;charset=UTF-8",
    "text/plain",
    "TEXT",
];
const MAX_OUTGOING: usize = 32;
const PASTE_CONFIRM: Duration = Duration::from_millis(1000);
/// Without a toplevel list, give the compositor time to refocus the previous window.
const PASTE_SETTLE: Duration = Duration::from_millis(250);
const KEYBOARD_SETTLE: Duration = Duration::from_millis(80);

// X keysyms and real modifier masks; these do not depend on the keyboard layout.
const XK_CONTROL_L: u32 = 0xffe3;
const XK_SHIFT_L: u32 = 0xffe1;
const XK_V: u32 = 0x0076;
const XK_INSERT: u32 = 0xff63;
const MOD_SHIFT: u32 = 1;
const MOD_CONTROL: u32 = 4;
/// A self-contained keymap for the virtual keyboard: evdev codes 1 and 2 (XKB 9 and 10) are
/// `v` and `Insert`. Shortcuts therefore resolve identically under every active user layout.
const KEYMAP: &str = "xkb_keymap {\n\
xkb_keycodes \"feritto\" { minimum = 8; maximum = 10; <FRTV> = 9; <FRTI> = 10; };\n\
xkb_types \"feritto\" { include \"complete\" };\n\
xkb_compatibility \"feritto\" { include \"complete\" };\n\
xkb_symbols \"feritto\" { key <FRTV> { [ v, V ] }; key <FRTI> { [ Insert ] }; };\n\
};\n";
const KEY_V: u32 = 1;
const KEY_INSERT: u32 = 2;

static INSTANCES: AtomicU64 = AtomicU64::new(0);

fn shortcut_name(shortcut: PasteShortcut) -> &'static str {
    match shortcut {
        PasteShortcut::ControlV => "Ctrl+V",
        PasteShortcut::ControlShiftV => "Ctrl+Shift+V",
        PasteShortcut::ShiftInsert => "Shift+Insert",
    }
}

/// Returns the depressed modifier mask, X keysyms of those modifiers, the virtual-keyboard
/// key code, and the key's keysym.
fn shortcut_keys(shortcut: PasteShortcut) -> (u32, Vec<u32>, u32, u32) {
    match shortcut {
        PasteShortcut::ControlV => (MOD_CONTROL, vec![XK_CONTROL_L], KEY_V, XK_V),
        PasteShortcut::ControlShiftV => (
            MOD_CONTROL | MOD_SHIFT,
            vec![XK_CONTROL_L, XK_SHIFT_L],
            KEY_V,
            XK_V,
        ),
        PasteShortcut::ShiftInsert => (MOD_SHIFT, vec![XK_SHIFT_L], KEY_INSERT, XK_INSERT),
    }
}

/// Maps a stored offer to the MIME types served on Wayland. Original names stay intact;
/// text is additionally offered under all common aliases, and Latin-1 text as `STRING`.
pub(crate) fn served_formats(offer: &Offer) -> Result<HashMap<String, Arc<[u8]>>> {
    let mut data = HashMap::new();
    for r in offer
        .representations
        .iter()
        .filter(|r| !is_protocol_target(&r.target))
    {
        data.insert(r.target.clone(), r.bytes.clone());
        if let Some(mime) = r.mime.as_ref().filter(|m| !is_protocol_target(m)) {
            data.entry(mime.clone()).or_insert_with(|| r.bytes.clone());
        }
    }
    if let Some(text) = offer.text_content() {
        let utf8: Arc<[u8]> = Arc::from(text.as_bytes());
        for name in TEXT_TARGETS {
            data.insert(name.to_owned(), utf8.clone());
        }
        if text.chars().all(|c| u32::from(c) <= 255) {
            data.insert(
                "STRING".into(),
                Arc::from(text.chars().map(|c| c as u8).collect::<Vec<_>>()),
            );
        } else {
            data.remove("STRING");
        }
    }
    if data.is_empty() {
        bail!("no supported formats in clipboard offer")
    }
    Ok(data)
}

// ---------------------------------------------------------------------------------------------
// Service facade

enum Command {
    Publish(
        Arc<Offer>,
        Option<(PasteTarget, PasteShortcut)>,
        Sender<ServiceResult<()>>,
    ),
    Clear(Sender<ServiceResult<()>>),
    Settings(Settings),
    Pause(bool),
    Active(Sender<ServiceResult<PasteTarget>>),
    Restore(PasteTarget),
    Text(Sender<ServiceResult<String>>),
    Portal(PortalUpdate),
    Shutdown(Sender<()>),
}

#[derive(Clone)]
struct ServiceSender {
    commands: Sender<Command>,
    wake: Arc<UnixDatagram>,
}
impl ServiceSender {
    fn send(&self, command: Command) -> ServiceResult<()> {
        self.commands
            .send(command)
            .map_err(|_| "Wayland worker stopped".to_owned())?;
        let _ = self.wake.send(&[1]);
        Ok(())
    }
    fn send_timeout(&self, command: Command, timeout: Duration) -> ServiceResult<()> {
        self.commands
            .send_timeout(command, timeout)
            .map_err(|_| "Wayland command queue is unavailable".to_owned())?;
        let _ = self.wake.send(&[1]);
        Ok(())
    }
}

#[derive(Clone)]
pub struct WaylandService {
    tx: ServiceSender,
    capabilities: Arc<Mutex<Capabilities>>,
    activation: Arc<Mutex<Option<String>>>,
    portal: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<PortalRequest>>>>,
}

impl WaylandService {
    pub fn start(
        settings: Settings,
        options: PlatformOptions,
    ) -> Result<(Self, Receiver<PlatformEvent>)> {
        let (commands, rx) = bounded(64);
        let (wake, worker_wake) = UnixDatagram::pair()?;
        wake.set_nonblocking(true)?;
        worker_wake.set_nonblocking(true)?;
        let tx = ServiceSender {
            commands,
            wake: Arc::new(wake),
        };
        let (events_tx, events_rx) = bounded(64);
        // A launcher may start the service with a token for its first window.
        let activation = Arc::new(Mutex::new(
            std::env::var("XDG_ACTIVATION_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
        ));
        let mut worker = Worker::connect(settings, events_tx, activation.clone())?;
        let capabilities = Arc::new(Mutex::new(worker.state.capabilities.clone()));
        worker.state.shared = Some(capabilities.clone());
        let portal = Arc::new(Mutex::new(None));
        // The FERITTO_NO_PORTALS escape hatch keeps headless tests and minimal sessions from
        // waiting on a D-Bus activation that cannot succeed.
        if std::env::var_os("FERITTO_NO_PORTALS").is_none() {
            let updates = tx.clone();
            let sender = portal::start(
                PortalOptions {
                    token_path: options.state_dir.join("remote-desktop.token"),
                    remote_desktop: worker.state.vk_manager.is_none(),
                    shortcuts: true,
                },
                move |update| {
                    let _ = updates.send(Command::Portal(update));
                },
            )?;
            worker.state.portal = Some(sender.clone());
            *portal.lock().unwrap() = Some(sender);
        } else {
            worker.state.portal_state(
                Capability::Unavailable(
                    "portals are disabled; bind `feritto --toggle` to a key in your desktop settings"
                        .into(),
                ),
                PasteAccess::Unavailable("portals are disabled".into()),
            );
        }
        thread::Builder::new()
            .name("feritto-wayland".into())
            .spawn(move || {
                if let Err(mut error) = worker.run(rx, worker_wake) {
                    if let Some(protocol) = worker.conn.protocol_error() {
                        error = error.context(format!(
                            "protocol error {} on {}@{}: {}",
                            protocol.code,
                            protocol.object_interface,
                            protocol.object_id,
                            protocol.message
                        ));
                    }
                    worker.state.emit(PlatformEvent::Status(format!(
                        "Wayland service stopped: {error}"
                    )));
                    tracing::error!(%error, "Wayland service stopped");
                }
            })?;
        Ok((
            Self {
                tx,
                capabilities,
                activation,
                portal,
            },
            events_rx,
        ))
    }
    pub fn capabilities(&self) -> Capabilities {
        self.capabilities.lock().unwrap().clone()
    }
    fn request<T>(
        &self,
        make: impl FnOnce(Sender<ServiceResult<T>>) -> Command,
    ) -> ServiceResult<T> {
        let (tx, rx) = bounded(1);
        self.tx
            .send_timeout(make(tx), Duration::from_secs(2))
            .map_err(|_| "Wayland worker unavailable".to_string())?;
        rx.recv_timeout(Duration::from_secs(3))
            .map_err(|_| "Wayland operation timed out".to_string())?
    }
    pub fn configure(&self, settings: Settings) {
        let _ = self.tx.send(Command::Settings(settings));
    }
    pub fn restore(&self, target: PasteTarget) {
        let _ = self.tx.send(Command::Restore(target));
    }
    pub fn shutdown(&self) {
        let (tx, rx) = bounded(1);
        if self.tx.send(Command::Shutdown(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
    }
    pub fn set_activation_token(&self, token: Option<String>) {
        if let Some(token) = token.filter(|t| !t.is_empty()) {
            *self.activation.lock().unwrap() = Some(token);
        }
    }
    /// Uses (and consumes) a pending activation token for a newly opened window. Returns
    /// whether a token was used; without one the compositor applies its own focus policy.
    pub fn activate_window(
        &self,
        display: RawDisplayHandle,
        window: RawWindowHandle,
    ) -> ServiceResult<bool> {
        let Some(token) = self.activation.lock().unwrap().take() else {
            return Ok(false);
        };
        activate_surface(display, window, &token).map_err(|e| format!("{e:#}"))?;
        Ok(true)
    }
    /// Text of the current clipboard when feritto owns it or has fully captured it. The GUI
    /// has no Wayland clipboard of its own, so its search field pastes through this.
    pub fn clipboard_text(&self) -> ServiceResult<String> {
        self.request(Command::Text)
    }
    pub fn request_paste_permission(&self) -> ServiceResult<()> {
        let portal = self.portal.lock().unwrap();
        let Some(portal) = portal.as_ref() else {
            return Err("The desktop portal is not available".into());
        };
        portal
            .send(PortalRequest::GrantPaste)
            .map_err(|_| "The desktop portal stopped".to_string())
    }
}

impl ClipboardMonitor for WaylandService {
    fn set_paused(&self, paused: bool) -> ServiceResult<()> {
        self.tx.send(Command::Pause(paused))
    }
}
impl ClipboardOwner for WaylandService {
    fn publish(&self, offer: Arc<Offer>) -> ServiceResult<()> {
        self.request(|reply| Command::Publish(offer, None, reply))
    }
    fn clear(&self) -> ServiceResult<()> {
        self.request(Command::Clear)
    }
}
impl HotkeyService for WaylandService {
    fn hotkey_capability(&self) -> Capability {
        self.capabilities().global_hotkey
    }
}
impl PasteInjector for WaylandService {
    fn paste(
        &self,
        offer: Arc<Offer>,
        target: PasteTarget,
        shortcut: PasteShortcut,
    ) -> ServiceResult<()> {
        self.request(|reply| Command::Publish(offer, Some((target, shortcut)), reply))
    }
}
impl ActiveWindowTracker for WaylandService {
    fn active_window(&self) -> ServiceResult<PasteTarget> {
        self.request(Command::Active)
    }
}

// ---------------------------------------------------------------------------------------------
// Protocol-family wrappers: ext-data-control and wlr-data-control have identical semantics.

enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}
enum Device {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}
enum Source {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}
#[derive(Clone)]
enum DataOffer {
    Ext(ExtDataControlOfferV1),
    Wlr(ZwlrDataControlOfferV1),
}

impl Manager {
    fn create_source(&self, qh: &QueueHandle<State>) -> Source {
        match self {
            Self::Ext(m) => Source::Ext(m.create_data_source(qh, ())),
            Self::Wlr(m) => Source::Wlr(m.create_data_source(qh, ())),
        }
    }
    fn device(&self, seat: &WlSeat, qh: &QueueHandle<State>) -> Device {
        match self {
            Self::Ext(m) => Device::Ext(m.get_data_device(seat, qh, ())),
            Self::Wlr(m) => Device::Wlr(m.get_data_device(seat, qh, ())),
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::Ext(_) => "ext-data-control",
            Self::Wlr(_) => "wlr-data-control",
        }
    }
}
impl Device {
    fn set_selection(&self, source: Option<&Source>, primary: bool) {
        match (self, source) {
            (Self::Ext(d), Some(Source::Ext(s))) if primary => d.set_primary_selection(Some(s)),
            (Self::Ext(d), Some(Source::Ext(s))) => d.set_selection(Some(s)),
            (Self::Ext(d), None) if primary => d.set_primary_selection(None),
            (Self::Ext(d), None) => d.set_selection(None),
            (Self::Wlr(d), Some(Source::Wlr(s))) if primary => d.set_primary_selection(Some(s)),
            (Self::Wlr(d), Some(Source::Wlr(s))) => d.set_selection(Some(s)),
            (Self::Wlr(d), None) if primary => d.set_primary_selection(None),
            (Self::Wlr(d), None) => d.set_selection(None),
            _ => unreachable!("sources and devices come from the same manager"),
        }
    }
}
impl Source {
    fn offer(&self, mime: String) {
        match self {
            Self::Ext(s) => s.offer(mime),
            Self::Wlr(s) => s.offer(mime),
        }
    }
    fn id(&self) -> ObjectId {
        match self {
            Self::Ext(s) => s.id(),
            Self::Wlr(s) => s.id(),
        }
    }
    fn destroy(&self) {
        match self {
            Self::Ext(s) => s.destroy(),
            Self::Wlr(s) => s.destroy(),
        }
    }
}
impl DataOffer {
    fn receive(&self, mime: String, fd: std::os::fd::BorrowedFd<'_>) {
        match self {
            Self::Ext(o) => o.receive(mime, fd),
            Self::Wlr(o) => o.receive(mime, fd),
        }
    }
    fn destroy(&self) {
        match self {
            Self::Ext(o) => o.destroy(),
            Self::Wlr(o) => o.destroy(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Worker state

struct OfferInfo {
    offer: DataOffer,
    mimes: Vec<String>,
}

struct Published {
    source: Source,
    data: Arc<HashMap<String, Arc<[u8]>>>,
}

struct Outgoing {
    fd: OwnedFd,
    bytes: Arc<[u8]>,
    offset: usize,
    progress: Instant,
}

struct Incoming {
    generation: u64,
    offer_id: ObjectId,
    offer: DataOffer,
    source: SourceIdentity,
    queue: VecDeque<String>,
    target: String,
    /// The current transfer reads the password-manager hint rather than content.
    hint: bool,
    pipe: Option<OwnedFd>,
    bytes: Vec<u8>,
    total: usize,
    representations: Vec<Representation>,
    omitted: Vec<String>,
    started: Instant,
    progress: Instant,
}

/// What a capture step decided; `settle` applies it so every path releases the pipe.
enum Step {
    Continue,
    Drop,
    Finish,
}

struct Saved {
    generation: u64,
    offer: Arc<Offer>,
}

struct Toplevel {
    handle: ZwlrForeignToplevelHandleV1,
    id: u32,
    app_id: Option<String>,
    activated: bool,
    pending_app_id: Option<String>,
    pending_activated: Option<bool>,
}
impl Toplevel {
    fn ours(&self) -> bool {
        self.app_id.as_deref() == Some(APP_ID)
    }
}

enum Injected {
    /// Keys were sent through the virtual keyboard.
    Sent,
    /// The portal thread sends the keys and reports the result.
    Pending,
}

struct PendingPaste {
    target: PasteTarget,
    shortcut: PasteShortcut,
    verify: bool,
    started: Instant,
    /// Clients learn about a new keyboard device asynchronously; keys sent before that are lost.
    not_before: Instant,
}

struct State {
    qh: QueueHandle<State>,
    events: Sender<PlatformEvent>,
    settings: Settings,
    marker: String,
    seat: Option<WlSeat>,
    manager: Option<Manager>,
    device: Option<Device>,
    primary_supported: bool,
    toplevel_manager: Option<ZwlrForeignToplevelManagerV1>,
    toplevels: HashMap<ObjectId, Toplevel>,
    next_toplevel: u32,
    last_foreign: Option<u32>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    keyboard: Option<ZwpVirtualKeyboardV1>,
    offers: HashMap<ObjectId, OfferInfo>,
    clipboard_offer: Option<ObjectId>,
    primary_offer: Option<ObjectId>,
    generation: u64,
    incoming: Option<Incoming>,
    saved: Option<Saved>,
    published: Option<Published>,
    primary_published: Option<Published>,
    outgoing: Vec<Outgoing>,
    /// Our own clear is in flight; its empty selection must not trigger recovery.
    expect_clear: bool,
    contention: VecDeque<Instant>,
    paste: Option<PendingPaste>,
    portal: Option<tokio::sync::mpsc::UnboundedSender<PortalRequest>>,
    hotkey: Capability,
    paste_access: PasteAccess,
    activation: Arc<Mutex<Option<String>>>,
    capabilities: Capabilities,
    shared: Option<Arc<Mutex<Capabilities>>>,
    clock: Instant,
}

struct Worker {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl Worker {
    fn connect(
        settings: Settings,
        events: Sender<PlatformEvent>,
        activation: Arc<Mutex<Option<String>>>,
    ) -> Result<Self> {
        let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).context("cannot read Wayland globals")?;
        let qh = queue.handle();
        let seat = globals.bind::<WlSeat, _, _>(&qh, 1..=7, ()).ok();
        let manager = bind_data_control(&globals, &qh);
        let primary_supported = match &manager {
            Some(Manager::Ext(_)) => true,
            Some(Manager::Wlr(m)) => m.version() >= 2,
            None => false,
        };
        let toplevel_manager = globals
            .bind::<ZwlrForeignToplevelManagerV1, _, _>(&qh, 1..=3, ())
            .ok();
        let vk_manager = globals
            .bind::<ZwpVirtualKeyboardManagerV1, _, _>(&qh, 1..=1, ())
            .ok();
        let instance = INSTANCES.fetch_add(1, Ordering::Relaxed);
        let mut state = State {
            qh: qh.clone(),
            events,
            settings,
            marker: format!("{OWNER_MARKER}-{}-{instance}", std::process::id()),
            device: match (&manager, &seat) {
                (Some(manager), Some(seat)) => Some(manager.device(seat, &qh)),
                _ => None,
            },
            seat,
            manager,
            primary_supported,
            toplevel_manager,
            toplevels: HashMap::new(),
            next_toplevel: 1,
            last_foreign: None,
            vk_manager,
            keyboard: None,
            offers: HashMap::new(),
            clipboard_offer: None,
            primary_offer: None,
            generation: 0,
            incoming: None,
            saved: None,
            published: None,
            primary_published: None,
            outgoing: vec![],
            expect_clear: false,
            contention: VecDeque::new(),
            paste: None,
            portal: None,
            hotkey: Capability::PermissionRequired(
                "waiting for the desktop's GlobalShortcuts portal".into(),
            ),
            paste_access: PasteAccess::Unavailable("waiting for the desktop portal".into()),
            activation,
            capabilities: Capabilities {
                backend: "Wayland".into(),
                monitor: Capability::Unavailable(String::new()),
                ownership: Capability::Unavailable(String::new()),
                global_hotkey: Capability::Unavailable(String::new()),
                automatic_paste: Capability::Unavailable(String::new()),
                source_identity: Capability::Unavailable(String::new()),
                pointer_placement: Capability::Unavailable(String::new()),
                caret_placement: Capability::Unavailable(String::new()),
                persistence: Capability::Unavailable(String::new()),
            },
            shared: None,
            clock: Instant::now(),
        };
        // Create the virtual keyboard up front: clients then already know the device and its
        // keymap when the first paste arrives, so no keys can precede their keyboard setup.
        if state.vk_manager.is_some() {
            match state.virtual_keyboard() {
                Ok(_) => state.paste_access = PasteAccess::Ready,
                Err(error) => {
                    tracing::warn!(%error, "virtual keyboard unavailable");
                    state.vk_manager = None;
                }
            }
        }
        // Receive the current selection and the initial toplevel list before reporting.
        queue.roundtrip(&mut state)?;
        state.update_capabilities();
        if state.device.is_none() {
            tracing::warn!(
                "the compositor offers no data-control protocol; clipboard history cannot observe copies"
            );
        }
        Ok(Self { conn, queue, state })
    }

    fn run(&mut self, rx: Receiver<Command>, wake: UnixDatagram) -> Result<()> {
        loop {
            self.queue.dispatch_pending(&mut self.state)?;
            let mut handled = 0;
            while handled < 64 {
                match rx.try_recv() {
                    Ok(Command::Shutdown(reply)) => {
                        self.orderly_shutdown();
                        let _ = reply.send(());
                        return Ok(());
                    }
                    Ok(command) => {
                        handled += 1;
                        self.command(command);
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => return Ok(()),
                }
            }
            self.state.pump_incoming();
            self.state.pump_outgoing();
            self.state.timeouts();
            match self.conn.flush() {
                Ok(()) => {}
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
            if handled > 0 {
                continue;
            }
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let busy = self.state.incoming.is_some()
                || !self.state.outgoing.is_empty()
                || self.state.paste.is_some();
            let timeout = if busy {
                rustix::event::Timespec {
                    tv_sec: 0,
                    tv_nsec: 20_000_000,
                }
            } else {
                rustix::event::Timespec {
                    tv_sec: 1,
                    tv_nsec: 0,
                }
            };
            let readable = {
                let display = guard.connection_fd();
                let mut fds = vec![
                    rustix::event::PollFd::new(&display, PollFlags::IN),
                    rustix::event::PollFd::new(&wake, PollFlags::IN),
                ];
                if let Some(pipe) = self.state.incoming.as_ref().and_then(|i| i.pipe.as_ref()) {
                    fds.push(rustix::event::PollFd::new(pipe, PollFlags::IN));
                }
                for out in &self.state.outgoing {
                    fds.push(rustix::event::PollFd::new(&out.fd, PollFlags::OUT));
                }
                match rustix::event::poll(&mut fds, Some(&timeout)) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(error) => return Err(error.into()),
                }
                !fds[0].revents().is_empty()
            };
            if readable {
                match guard.read() {
                    Ok(_) => {}
                    Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error.into()),
                }
            } else {
                drop(guard);
            }
            let mut bytes = [0; 64];
            while wake.recv(&mut bytes).is_ok() {}
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Publish(offer, paste, reply) => {
                let primary = paste.is_some_and(|(_, s)| matches!(s, PasteShortcut::ShiftInsert));
                let mut result = self.state.publish(&offer, primary);
                if result.is_ok() {
                    // A refused source is cancelled immediately; confirm before reporting.
                    if let Err(error) = self.queue.roundtrip(&mut self.state) {
                        result = Err(error.into());
                    } else if self.state.published.is_none() {
                        result = Err(anyhow!("clipboard ownership was refused"));
                    }
                }
                let success = result.is_ok();
                let _ = reply.send(result.map_err(|e| format!("{e:#}")));
                if success && let Some((target, shortcut)) = paste {
                    self.state.begin_paste(target, shortcut);
                }
            }
            Command::Clear(reply) => {
                let _ = reply.send(self.state.clear().map_err(|e| format!("{e:#}")));
            }
            Command::Pause(paused) => {
                self.state.settings.paused = paused;
                if paused {
                    self.state.cancel_incoming();
                    self.state.saved = None;
                }
            }
            Command::Settings(settings) => self.state.configure(settings),
            Command::Active(reply) => {
                let _ = reply.send(Ok(self.state.target()));
            }
            Command::Restore(target) => self.state.restore(target),
            Command::Text(reply) => {
                let _ = reply.send(
                    self.state
                        .current_text()
                        .ok_or_else(|| "The clipboard holds no captured text".to_string()),
                );
            }
            Command::Portal(update) => self.state.portal_update(update),
            Command::Shutdown(_) => {}
        }
    }

    fn orderly_shutdown(&mut self) {
        if self.state.settings.clear_clipboard_on_exit
            && let Some(device) = &self.state.device
        {
            device.set_selection(None, false);
        }
        // Wayland has no clipboard-manager handoff: a selection we own ends with this process
        // unless another manager has already copied it.
        let _ = self.queue.roundtrip(&mut self.state);
        let _ = self.conn.flush();
    }
}

fn bind_data_control(globals: &GlobalList, qh: &QueueHandle<State>) -> Option<Manager> {
    if let Ok(m) = globals.bind::<ExtDataControlManagerV1, _, _>(qh, 1..=1, ()) {
        return Some(Manager::Ext(m));
    }
    globals
        .bind::<ZwlrDataControlManagerV1, _, _>(qh, 1..=2, ())
        .ok()
        .map(Manager::Wlr)
}

impl State {
    fn emit(&self, event: PlatformEvent) {
        if self.events.try_send(event).is_err() {
            tracing::warn!("platform event queue is full; dropped an event");
        }
    }

    fn report(&self, error: anyhow::Error) {
        tracing::warn!(%error, "Wayland operation failed");
        self.emit(PlatformEvent::Status(format!(
            "Wayland operation failed: {error:#}"
        )));
    }

    fn update_capabilities(&mut self) {
        let mut protocols: Vec<&str> = vec![];
        if let Some(manager) = &self.manager {
            protocols.push(manager.name());
        }
        if self.toplevel_manager.is_some() {
            protocols.push("wlr-foreign-toplevel");
        }
        if self.vk_manager.is_some() {
            protocols.push("virtual-keyboard");
        }
        let backend = if protocols.is_empty() {
            "Wayland".to_owned()
        } else {
            format!("Wayland ({})", protocols.join(", "))
        };
        let no_control = || {
            Capability::Unavailable(
                "the compositor offers neither ext-data-control nor wlr-data-control".into(),
            )
        };
        let tracking = self.toplevel_manager.is_some();
        let automatic_paste = match &self.paste_access {
            PasteAccess::Unavailable(reason) => Capability::Unavailable(format!(
                "no virtual-keyboard protocol or RemoteDesktop portal keyboard ({reason})"
            )),
            PasteAccess::PermissionRequired(reason) => {
                Capability::PermissionRequired(reason.clone())
            }
            PasteAccess::Ready if self.device.is_none() => no_control(),
            PasteAccess::Ready if !tracking && !self.settings.unverified_paste => {
                Capability::Unavailable(
                    "the compositor cannot confirm the target window; allow unverified paste in Settings to send keys anyway"
                        .into(),
                )
            }
            PasteAccess::Ready => Capability::Available,
        };
        self.capabilities = Capabilities {
            backend,
            monitor: if self.device.is_some() {
                Capability::Available
            } else {
                no_control()
            },
            ownership: if self.device.is_some() {
                Capability::Available
            } else {
                no_control()
            },
            global_hotkey: self.hotkey.clone(),
            automatic_paste,
            source_identity: if tracking {
                Capability::Available
            } else {
                Capability::Unavailable(
                    "the compositor does not expose the focused application; application exclusions are inactive"
                        .into(),
                )
            },
            pointer_placement: Capability::Unavailable(
                "Wayland compositors place the popup".into(),
            ),
            caret_placement: Capability::Unavailable(
                "Wayland does not expose another application's text caret".into(),
            ),
            persistence: if self.device.is_none() {
                no_control()
            } else if self.settings.coexistence {
                Capability::Unavailable("history-only mode disables recovery".into())
            } else {
                Capability::Available
            },
        };
        if let Some(shared) = &self.shared {
            *shared.lock().unwrap() = self.capabilities.clone();
        }
    }

    fn portal_state(&mut self, hotkey: Capability, paste: PasteAccess) {
        self.hotkey = hotkey;
        if self.vk_manager.is_none() {
            self.paste_access = paste;
        }
        self.update_capabilities();
    }

    fn portal_update(&mut self, update: PortalUpdate) {
        match update {
            PortalUpdate::Hotkey(capability) => {
                self.hotkey = capability;
                self.update_capabilities();
            }
            PortalUpdate::Paste(access) => {
                if self.vk_manager.is_none() {
                    self.paste_access = access;
                    self.update_capabilities();
                }
            }
            PortalUpdate::Activated(token) => {
                if let Some(token) = token {
                    *self.activation.lock().unwrap() = Some(token);
                }
                self.emit(PlatformEvent::Toggle(self.target()));
            }
            PortalUpdate::Injected(Ok(())) => {
                self.emit(PlatformEvent::PasteResult(
                    "Pasted into the previous window".into(),
                ));
            }
            PortalUpdate::Injected(Err(error)) => {
                self.emit(PlatformEvent::PasteResult(format!(
                    "Copied; press your paste shortcut ({error})"
                )));
            }
        }
    }

    fn configure(&mut self, settings: Settings) {
        if settings.paused || settings.capture != self.settings.capture {
            self.cancel_incoming();
            self.saved = None;
        }
        self.settings = settings;
        self.update_capabilities();
    }

    // -- Selection monitoring ----------------------------------------------------------------

    fn selection(&mut self, id: Option<ObjectId>, primary: bool) {
        if primary {
            // PRIMARY is served on request but not monitored in the background.
            let old = std::mem::replace(&mut self.primary_offer, id);
            self.forget_offer(old);
            return;
        }
        self.cancel_incoming();
        let old = std::mem::replace(&mut self.clipboard_offer, id.clone());
        self.forget_offer(old);
        let Some(id) = id else {
            self.selection_cleared();
            return;
        };
        self.expect_clear = false;
        // Every new generation invalidates older recovery data, captured or not.
        self.generation = self.generation.wrapping_add(1);
        self.saved = None;
        let Some(info) = self.offers.get(&id) else {
            return;
        };
        if info.mimes.contains(&self.marker) {
            return;
        }
        if self.settings.paused
            || info
                .mimes
                .iter()
                .any(|m| IGNORE_HINTS.contains(&m.as_str()))
        {
            return;
        }
        let source = self.source_identity();
        let excluded = source.application.as_ref().is_some_and(|app| {
            self.settings
                .capture
                .excluded_applications
                .iter()
                .any(|excluded| app.eq_ignore_ascii_case(excluded))
        });
        if excluded {
            return;
        }
        let mut queue: VecDeque<String> = self
            .settings
            .capture
            .select_targets(&info.mimes)
            .into_iter()
            .collect();
        if queue.is_empty() {
            return;
        }
        let hint = info.mimes.iter().any(|m| m == SECRET_HINT);
        if hint {
            queue.push_front(SECRET_HINT.into());
        }
        let now = Instant::now();
        let mut input = Incoming {
            generation: self.generation,
            offer_id: id,
            offer: info.offer.clone(),
            source,
            queue,
            target: String::new(),
            hint: false,
            pipe: None,
            bytes: vec![],
            total: 0,
            representations: vec![],
            omitted: vec![],
            started: now,
            progress: now,
        };
        let step = self.next_target(&mut input);
        self.settle(input, step);
    }

    /// The selection became empty: either its source exited or a client cleared it. Wayland
    /// does not distinguish the two, so a fully captured current generation is restored unless
    /// this process cleared it.
    fn selection_cleared(&mut self) {
        let saved = self.saved.take();
        if std::mem::take(&mut self.expect_clear) {
            return;
        }
        if self.device.is_none() || self.settings.paused || self.settings.coexistence {
            return;
        }
        if let Some(saved) = saved
            && saved.generation == self.generation
            && let Err(error) = self.publish(&saved.offer, false)
        {
            self.report(error.context("could not restore the clipboard after its source exited"));
        }
    }

    fn forget_offer(&mut self, id: Option<ObjectId>) {
        if let Some(id) = id
            && Some(&id) != self.clipboard_offer.as_ref()
            && Some(&id) != self.primary_offer.as_ref()
            && let Some(info) = self.offers.remove(&id)
        {
            info.offer.destroy();
        }
    }

    fn source_identity(&self) -> SourceIdentity {
        let active = self.toplevels.values().find(|t| t.activated && !t.ours());
        SourceIdentity {
            application: active.and_then(|t| t.app_id.clone()),
            window: active.map(|t| u64::from(t.id)),
        }
    }

    fn cancel_incoming(&mut self) {
        self.incoming = None;
    }

    fn settle(&mut self, input: Incoming, step: Result<Step>) {
        match step {
            Ok(Step::Continue) => self.incoming = Some(input),
            Ok(Step::Drop) => {}
            Ok(Step::Finish) => self.finish_capture(input),
            Err(error) => self.report(error),
        }
    }

    fn next_target(&mut self, input: &mut Incoming) -> Result<Step> {
        let Some(target) = input.queue.pop_front() else {
            return Ok(Step::Finish);
        };
        let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        // Only our end is non-blocking; the source writes to a normal blocking pipe.
        rustix::io::ioctl_fionbio(&read, true)?;
        input.offer.receive(target.clone(), write.as_fd());
        // The request holds its own duplicate of the descriptor until it is sent.
        drop(write);
        input.hint = target == SECRET_HINT;
        input.target = target;
        input.pipe = Some(read);
        input.bytes.clear();
        input.progress = Instant::now();
        Ok(Step::Continue)
    }

    fn pump_incoming(&mut self) {
        let Some(mut input) = self.incoming.take() else {
            return;
        };
        let step = self.read_incoming(&mut input);
        self.settle(input, step);
    }

    fn read_incoming(&mut self, input: &mut Incoming) -> Result<Step> {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let Some(pipe) = &input.pipe else {
                return Ok(Step::Continue);
            };
            match rustix::io::read(pipe, &mut buffer) {
                Ok(0) => return self.target_done(input),
                Ok(n) => {
                    input.progress = Instant::now();
                    input.bytes.extend_from_slice(&buffer[..n]);
                    let limit = if input.hint {
                        64
                    } else {
                        self.settings.capture.max_target_bytes
                    };
                    if input.bytes.len() > limit
                        || input.total + input.bytes.len()
                            > self.settings.capture.max_generation_bytes
                    {
                        if input.hint {
                            // An oversized hint is treated as secret.
                            return Ok(Step::Drop);
                        }
                        input.pipe = None;
                        input.omitted.push(input.target.clone());
                        input.bytes = vec![];
                        return self.next_target(input);
                    }
                }
                Err(Errno::AGAIN) => return Ok(Step::Continue),
                Err(Errno::INTR) => {}
                Err(error) => {
                    input.pipe = None;
                    if input.hint {
                        return Ok(Step::Drop);
                    }
                    tracing::debug!(%error, target = %input.target, "clipboard transfer failed");
                    input.omitted.push(input.target.clone());
                    return self.next_target(input);
                }
            }
        }
    }

    fn target_done(&mut self, input: &mut Incoming) -> Result<Step> {
        input.pipe = None;
        let bytes = std::mem::take(&mut input.bytes);
        if input.hint {
            let value = String::from_utf8_lossy(&bytes);
            if value
                .trim_end_matches('\0')
                .trim()
                .eq_ignore_ascii_case("secret")
            {
                // Secret generations are never fetched, stored, or recoverable.
                return Ok(Step::Drop);
            }
            return self.next_target(input);
        }
        if bytes.is_empty() {
            // An empty transfer is a source that declined this format.
            input.omitted.push(input.target.clone());
            return self.next_target(input);
        }
        let target = input.target.clone();
        let mime = if is_text(&target) {
            Some("text/plain;charset=utf-8".into())
        } else if target.contains('/') {
            Some(target.clone())
        } else {
            None
        };
        input.total += bytes.len();
        input.representations.push(Representation {
            target,
            mime,
            bytes: Arc::from(bytes),
        });
        self.next_target(input)
    }

    fn finish_capture(&mut self, input: Incoming) {
        if input.representations.is_empty() {
            return;
        }
        if input.generation != self.generation
            || self.clipboard_offer.as_ref() != Some(&input.offer_id)
        {
            return;
        }
        let complete = input.omitted.is_empty() && input.queue.is_empty();
        let offer = Arc::new(Offer {
            representations: input.representations,
        });
        if complete {
            self.saved = Some(Saved {
                generation: input.generation,
                offer: offer.clone(),
            });
        }
        self.emit(PlatformEvent::Captured(Capture {
            generation: input.generation,
            offer,
            source: input.source,
            complete,
            omitted: input.omitted,
        }));
        if !complete {
            self.emit(PlatformEvent::Status(
                "Clipboard capture was incomplete; automatic recovery is disabled for this copy"
                    .into(),
            ));
        }
    }

    // -- Selection ownership -----------------------------------------------------------------

    fn create_published(&self, data: &Arc<HashMap<String, Arc<[u8]>>>) -> Result<Published> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| anyhow!("the compositor offers no data-control protocol"))?;
        let source = manager.create_source(&self.qh);
        let mut names: Vec<_> = data.keys().cloned().collect();
        names.sort();
        for name in names {
            source.offer(name);
        }
        source.offer(self.marker.clone());
        Ok(Published {
            source,
            data: data.clone(),
        })
    }

    fn publish(&mut self, offer: &Offer, primary: bool) -> Result<()> {
        if self.device.is_none() {
            bail!(
                "the compositor offers no data-control protocol; clipboard ownership is unavailable"
            );
        }
        let data = Arc::new(served_formats(offer)?);
        let published = self.create_published(&data)?;
        let device = self.device.as_ref().unwrap();
        // Replaced sources are destroyed rather than cancelled, so they never look like
        // contention with another manager.
        if let Some(old) = self.published.take() {
            old.source.destroy();
        }
        device.set_selection(Some(&published.source), false);
        self.published = Some(published);
        if primary && self.primary_supported {
            let published = self.create_published(&data)?;
            if let Some(old) = self.primary_published.take() {
                old.source.destroy();
            }
            self.device
                .as_ref()
                .unwrap()
                .set_selection(Some(&published.source), true);
            self.primary_published = Some(published);
        }
        self.expect_clear = false;
        self.saved = None;
        self.cancel_incoming();
        Ok(())
    }

    fn clear(&mut self) -> Result<()> {
        if self.device.is_none() {
            bail!("the compositor offers no data-control protocol");
        }
        self.cancel_incoming();
        self.saved = None;
        if let Some(old) = self.published.take() {
            old.source.destroy();
        }
        self.device.as_ref().unwrap().set_selection(None, false);
        self.expect_clear = true;
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    fn send(&mut self, source: ObjectId, mime: String, fd: OwnedFd) {
        let data = [&self.published, &self.primary_published]
            .into_iter()
            .flatten()
            .find(|p| p.source.id() == source)
            .and_then(|p| p.data.get(&mime).cloned());
        // Closing the descriptor without data tells the requestor the format is unavailable.
        let Some(bytes) = data else {
            return;
        };
        if self.outgoing.len() >= MAX_OUTGOING {
            tracing::debug!("too many concurrent clipboard requests; declined one");
            return;
        }
        if rustix::io::ioctl_fionbio(&fd, true).is_err() {
            return;
        }
        self.outgoing.push(Outgoing {
            fd,
            bytes,
            offset: 0,
            progress: Instant::now(),
        });
    }

    fn pump_outgoing(&mut self) {
        let now = Instant::now();
        self.outgoing.retain_mut(|out| {
            while out.offset < out.bytes.len() {
                let end = (out.offset + 64 * 1024).min(out.bytes.len());
                match rustix::io::write(&out.fd, &out.bytes[out.offset..end]) {
                    Ok(n) => {
                        out.offset += n;
                        out.progress = now;
                    }
                    Err(Errno::AGAIN) => {
                        return now.duration_since(out.progress) < Duration::from_secs(10);
                    }
                    Err(Errno::INTR) => {}
                    // EPIPE: the requestor stopped reading. SIGPIPE is ignored by Rust programs.
                    Err(_) => return false,
                }
            }
            false
        });
    }

    fn cancelled(&mut self, source: ObjectId) {
        if self
            .primary_published
            .as_ref()
            .is_some_and(|p| p.source.id() == source)
        {
            let old = self.primary_published.take().unwrap();
            old.source.destroy();
            return;
        }
        if !self
            .published
            .as_ref()
            .is_some_and(|p| p.source.id() == source)
        {
            return;
        }
        let old = self.published.take().unwrap();
        old.source.destroy();
        // Another client replaced our selection. Repeated takeovers within a short window mean
        // another manager is fighting for ownership; stop recovering to end the loop.
        let now = Instant::now();
        self.contention.push_back(now);
        while self
            .contention
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(2))
        {
            self.contention.pop_front();
        }
        if self.contention.len() >= 3 && !self.settings.coexistence {
            self.settings.coexistence = true;
            self.update_capabilities();
            self.capabilities.persistence =
                Capability::Unavailable("Ownership contention detected; recovery disabled".into());
            if let Some(shared) = &self.shared {
                *shared.lock().unwrap() = self.capabilities.clone();
            }
            self.emit(PlatformEvent::Status(
                "Clipboard ownership contention detected; switched to history-only capture".into(),
            ));
        }
    }

    // -- Windows and paste -------------------------------------------------------------------

    fn toplevel(&self, id: u32) -> Option<&Toplevel> {
        self.toplevels.values().find(|t| t.id == id)
    }

    /// The window that should receive a paste: the active window, or the one that was active
    /// just before feritto's own window. 0 means unknown.
    fn target(&self) -> PasteTarget {
        let window = match self.toplevels.values().find(|t| t.activated) {
            Some(t) if !t.ours() => t.id,
            Some(_) => self.last_foreign.unwrap_or(0),
            None => 0,
        };
        PasteTarget {
            window,
            timestamp: 0,
        }
    }

    fn current_text(&self) -> Option<String> {
        if let Some(published) = &self.published {
            return published
                .data
                .get(TEXT_TARGETS[0])
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
        }
        self.saved
            .as_ref()
            .filter(|saved| saved.generation == self.generation)
            .and_then(|saved| saved.offer.text_content())
    }

    fn restore(&mut self, target: PasteTarget) {
        if let (Some(toplevel), Some(seat)) = (self.toplevel(target.window), &self.seat) {
            toplevel.handle.activate(seat);
        }
    }

    fn begin_paste(&mut self, target: PasteTarget, shortcut: PasteShortcut) {
        let name = shortcut_name(shortcut);
        let reason = match &self.paste_access {
            PasteAccess::Ready => None,
            PasteAccess::PermissionRequired(reason) | PasteAccess::Unavailable(reason) => {
                Some(reason.clone())
            }
        };
        if let Some(reason) = reason {
            self.emit(PlatformEvent::PasteResult(format!(
                "Copied; press {name} to paste (automatic paste: {reason})"
            )));
            return;
        }
        let verify = self.toplevel_manager.is_some() && self.toplevel(target.window).is_some();
        if !verify && !self.settings.unverified_paste {
            self.emit(PlatformEvent::PasteResult(format!(
                "Copied; press {name} to paste (the target window cannot be confirmed)"
            )));
            return;
        }
        if verify {
            self.restore(target);
        }
        let now = Instant::now();
        let mut not_before = now;
        if self.vk_manager.is_some() && self.keyboard.is_none() {
            if let Err(error) = self.virtual_keyboard() {
                self.emit(PlatformEvent::PasteResult(format!(
                    "Copied; press {name} to paste ({error:#})"
                )));
                return;
            }
            not_before = now + KEYBOARD_SETTLE;
        }
        self.paste = Some(PendingPaste {
            target,
            shortcut,
            verify,
            started: now,
            not_before,
        });
    }

    /// Returns the final paste status, or None to retry on the next tick.
    fn paste_tick(&mut self, now: Instant) -> Option<String> {
        let paste = self.paste.as_ref()?;
        let name = shortcut_name(paste.shortcut);
        if self.published.is_none() {
            return Some("Clipboard changed before paste; no keys were sent".into());
        }
        if now < paste.not_before {
            return None;
        }
        let our_window_active = self.toplevels.values().any(|t| t.activated && t.ours());
        let ready = if paste.verify {
            self.toplevel(paste.target.window)
                .is_some_and(|t| t.activated)
        } else {
            now.duration_since(paste.started) >= PASTE_SETTLE && !our_window_active
        };
        if ready {
            let shortcut = paste.shortcut;
            return match self.inject(shortcut) {
                Ok(Injected::Sent) => Some("Pasted into the previous window".into()),
                Ok(Injected::Pending) => {
                    // The portal thread reports the result; stop tracking the paste here.
                    self.paste = None;
                    None
                }
                Err(error) => Some(format!("Copied; press {name} to paste ({error:#})")),
            };
        }
        if now.duration_since(paste.started) >= PASTE_CONFIRM {
            return Some(format!(
                "Copied; press {name} to paste (the target window did not regain focus)"
            ));
        }
        None
    }

    fn inject(&mut self, shortcut: PasteShortcut) -> Result<Injected> {
        let (mask, modifiers, code, keysym) = shortcut_keys(shortcut);
        if self.vk_manager.is_some() {
            let time = self.clock.elapsed().as_millis() as u32;
            let keyboard = self.virtual_keyboard()?;
            keyboard.modifiers(mask, 0, 0, 0);
            keyboard.key(time, code, 1);
            keyboard.key(time.wrapping_add(1), code, 0);
            keyboard.modifiers(0, 0, 0, 0);
            return Ok(Injected::Sent);
        }
        let portal = self
            .portal
            .as_ref()
            .ok_or_else(|| anyhow!("no keyboard injection is available"))?;
        portal
            .send(PortalRequest::Inject {
                modifiers,
                key: keysym,
            })
            .map_err(|_| anyhow!("the desktop portal stopped"))?;
        Ok(Injected::Pending)
    }

    fn virtual_keyboard(&mut self) -> Result<&ZwpVirtualKeyboardV1> {
        if self.keyboard.is_none() {
            let manager = self
                .vk_manager
                .as_ref()
                .ok_or_else(|| anyhow!("no virtual-keyboard protocol"))?;
            let seat = self.seat.as_ref().ok_or_else(|| anyhow!("no seat"))?;
            let keyboard = manager.create_virtual_keyboard(seat, &self.qh, ());
            let fd = rustix::fs::memfd_create("feritto-keymap", rustix::fs::MemfdFlags::CLOEXEC)?;
            let mut file = std::fs::File::from(fd);
            file.write_all(KEYMAP.as_bytes())?;
            file.write_all(&[0])?;
            // wl_keyboard keymap format 1 is xkb_v1; the size includes the terminating NUL.
            keyboard.keymap(1, file.as_fd(), KEYMAP.len() as u32 + 1);
            self.keyboard = Some(keyboard);
        }
        Ok(self.keyboard.as_ref().unwrap())
    }

    fn timeouts(&mut self) {
        let now = Instant::now();
        if self.incoming.as_ref().is_some_and(|input| {
            now.duration_since(input.progress)
                > Duration::from_millis(self.settings.capture.progress_timeout_ms)
                || now.duration_since(input.started)
                    > Duration::from_millis(self.settings.capture.generation_timeout_ms)
        }) {
            let mut input = self.incoming.take().unwrap();
            input.pipe = None;
            if !input.hint {
                input.omitted.push(input.target.clone());
                self.finish_capture(input);
            }
        }
        if self.paste.is_some()
            && let Some(status) = self.paste_tick(now)
        {
            self.paste = None;
            self.emit(PlatformEvent::PasteResult(status));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Dispatch

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

macro_rules! ignore_events {
    ($($interface:ty),* $(,)?) => {$(
        impl Dispatch<$interface, ()> for State {
            fn event(
                _: &mut Self,
                _: &$interface,
                _: <$interface as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}
ignore_events!(
    WlSeat,
    ExtDataControlManagerV1,
    ZwlrDataControlManagerV1,
    ZwpVirtualKeyboardManagerV1,
    ZwpVirtualKeyboardV1,
);

macro_rules! data_control_dispatch {
    ($device:ident, $Device:ty, $offer:ident, $Offer:ty, $source:ident, $Source:ty, $Wrap:ident) => {
        impl Dispatch<$Device, ()> for State {
            fn event(
                state: &mut Self,
                _: &$Device,
                event: $device::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $device::Event::DataOffer { id } => {
                        state.offers.insert(
                            id.id(),
                            OfferInfo {
                                offer: DataOffer::$Wrap(id),
                                mimes: vec![],
                            },
                        );
                    }
                    $device::Event::Selection { id } => state.selection(id.map(|o| o.id()), false),
                    $device::Event::PrimarySelection { id } => {
                        state.selection(id.map(|o| o.id()), true)
                    }
                    $device::Event::Finished => {
                        state.device = None;
                        state.published = None;
                        state.primary_published = None;
                        state.cancel_incoming();
                        state.update_capabilities();
                        state.emit(PlatformEvent::Status(
                            "The compositor stopped clipboard access; history capture is unavailable"
                                .into(),
                        ));
                    }
                    _ => {}
                }
            }
            event_created_child!(State, $Device, [
                $device::EVT_DATA_OFFER_OPCODE => ($Offer, ()),
            ]);
        }
        impl Dispatch<$Offer, ()> for State {
            fn event(
                state: &mut Self,
                offer: &$Offer,
                event: $offer::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let $offer::Event::Offer { mime_type } = event
                    && let Some(info) = state.offers.get_mut(&offer.id())
                    && info.mimes.len() < 512
                {
                    info.mimes.push(mime_type);
                }
            }
        }
        impl Dispatch<$Source, ()> for State {
            fn event(
                state: &mut Self,
                source: &$Source,
                event: $source::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $source::Event::Send { mime_type, fd } => state.send(source.id(), mime_type, fd),
                    $source::Event::Cancelled => state.cancelled(source.id()),
                    _ => {}
                }
            }
        }
    };
}
data_control_dispatch!(
    ext_device,
    ExtDataControlDeviceV1,
    ext_offer,
    ExtDataControlOfferV1,
    ext_source,
    ExtDataControlSourceV1,
    Ext
);
data_control_dispatch!(
    wlr_device,
    ZwlrDataControlDeviceV1,
    wlr_offer,
    ZwlrDataControlOfferV1,
    wlr_source,
    ZwlrDataControlSourceV1,
    Wlr
);

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: toplevel_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            toplevel_manager::Event::Toplevel { toplevel } => {
                let id = state.next_toplevel;
                state.next_toplevel = state.next_toplevel.wrapping_add(1).max(1);
                state.toplevels.insert(
                    toplevel.id(),
                    Toplevel {
                        handle: toplevel,
                        id,
                        app_id: None,
                        activated: false,
                        pending_app_id: None,
                        pending_activated: None,
                    },
                );
            }
            toplevel_manager::Event::Finished => {
                state.toplevel_manager = None;
                state.toplevels.clear();
                state.update_capabilities();
            }
            _ => {}
        }
    }
    event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        toplevel_manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: toplevel_handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let key = handle.id();
        match event {
            toplevel_handle::Event::AppId { app_id } => {
                if let Some(t) = state.toplevels.get_mut(&key) {
                    t.pending_app_id = Some(app_id);
                }
            }
            toplevel_handle::Event::State { state: raw } => {
                let activated = raw
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .any(|s| s == toplevel_handle::State::Activated as u32);
                if let Some(t) = state.toplevels.get_mut(&key) {
                    t.pending_activated = Some(activated);
                }
            }
            toplevel_handle::Event::Done => {
                if let Some(t) = state.toplevels.get_mut(&key) {
                    if let Some(app_id) = t.pending_app_id.take() {
                        t.app_id = Some(app_id);
                    }
                    if let Some(activated) = t.pending_activated.take() {
                        t.activated = activated;
                    }
                    if t.activated && !t.ours() {
                        state.last_foreign = Some(t.id);
                    }
                }
            }
            toplevel_handle::Event::Closed => {
                if let Some(t) = state.toplevels.remove(&key) {
                    if state.last_foreign == Some(t.id) {
                        state.last_foreign = None;
                    }
                    t.handle.destroy();
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// xdg-activation for windows created by the GUI toolkit

struct Activation;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Activation {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<XdgActivationV1, ()> for Activation {
    fn event(
        _: &mut Self,
        _: &XdgActivationV1,
        _: <XdgActivationV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<XdgActivationTokenV1, ()> for Activation {
    fn event(
        _: &mut Self,
        _: &XdgActivationTokenV1,
        _: <XdgActivationTokenV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Activates a toolkit-owned surface with an xdg-activation token. The toolkit's display is
/// borrowed through libwayland on a private event queue, so its own dispatching is unaffected.
/// Call it on the toolkit's thread while the window is alive.
pub fn activate_surface(
    display: RawDisplayHandle,
    window: RawWindowHandle,
    token: &str,
) -> Result<()> {
    let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(window)) = (display, window)
    else {
        bail!("the window does not use the Wayland backend");
    };
    // SAFETY: the display pointer comes from the live toolkit window and outlives this call;
    // the borrowed backend is dropped before returning and never closes the display.
    let backend = unsafe {
        wayland_backend::client::Backend::from_foreign_display(display.display.as_ptr().cast())
    };
    let conn = Connection::from_backend(backend);
    // SAFETY: the surface pointer is the toolkit's live wl_surface proxy on that display.
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), window.surface.as_ptr().cast()) }
        .map_err(|_| anyhow!("invalid Wayland surface handle"))?;
    let surface = WlSurface::from_id(&conn, id).map_err(|_| anyhow!("invalid Wayland surface"))?;
    let (globals, mut queue) = registry_queue_init::<Activation>(&conn)?;
    let activation = globals
        .bind::<XdgActivationV1, _, _>(&queue.handle(), 1..=1, ())
        .context("the compositor does not support xdg-activation")?;
    activation.activate(token.to_owned(), &surface);
    activation.destroy();
    queue.roundtrip(&mut Activation)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn served_formats_keep_originals_and_add_text_aliases() {
        let mut offer = Offer::text("café");
        offer.representations.push(Representation {
            target: "text/html".into(),
            mime: Some("text/html".into()),
            bytes: Arc::from(b"<b>caf\xc3\xa9</b>".as_slice()),
        });
        offer.representations.push(Representation {
            target: "x-kde-passwordManagerHint".into(),
            mime: None,
            bytes: Arc::from(b"secret".as_slice()),
        });
        let data = served_formats(&offer).unwrap();
        for name in TEXT_TARGETS {
            assert_eq!(&*data[name], "café".as_bytes(), "{name}");
        }
        assert_eq!(&*data["STRING"], b"caf\xe9");
        assert_eq!(&*data["text/html"], b"<b>caf\xc3\xa9</b>");
        assert!(!data.contains_key("x-kde-passwordManagerHint"));
        let wide = served_formats(&Offer::text("😀")).unwrap();
        assert!(!wide.contains_key("STRING"));
    }

    #[test]
    fn shortcut_keys_are_layout_independent() {
        assert_eq!(
            shortcut_keys(PasteShortcut::ControlV),
            (MOD_CONTROL, vec![XK_CONTROL_L], KEY_V, XK_V)
        );
        assert_eq!(
            shortcut_keys(PasteShortcut::ShiftInsert),
            (MOD_SHIFT, vec![XK_SHIFT_L], KEY_INSERT, XK_INSERT)
        );
        assert!(KEYMAP.contains("<FRTV> = 9") && KEYMAP.contains("<FRTI> = 10"));
    }
}
