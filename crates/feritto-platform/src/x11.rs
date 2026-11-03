use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender, bounded};
use feritto_core::*;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE,
    connection::{Connection, RequestConnection},
    protocol::{
        Event,
        xfixes::{self, ConnectionExt as _},
        xproto::*,
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        CLIPBOARD, CLIPBOARD_MANAGER, TARGETS, TIMESTAMP, MULTIPLE, ATOM_PAIR, INCR, SAVE_TARGETS,
        UTF8_STRING, STRING, INTEGER, ATOM,
        _NET_ACTIVE_WINDOW, _NET_WM_WINDOW_TYPE, _NET_WM_WINDOW_TYPE_UTILITY,
        _NET_WM_STATE, _NET_WM_STATE_SKIP_TASKBAR, _NET_WM_STATE_SKIP_PAGER, _NET_WM_STATE_ABOVE,
        _NET_WM_USER_TIME, _NET_WM_NAME, _NET_WM_PID, _NET_CURRENT_DESKTOP, _NET_WORKAREA,
        WM_CLASS, WM_TRANSIENT_FOR,
        _FERITTO_TIME,
        KDE_SECRET: b"x-kde-passwordManagerHint",
        IGNORE: b"application/x-feritto-ignore",
        VIEWER_IGNORE: b"Clipboard Viewer Ignore",
    }
}

#[derive(Debug, Clone)]
pub enum PlatformEvent {
    Captured(Capture),
    Toggle(PasteTarget),
    Status(String),
    PasteResult(String),
}

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
    Prepare(u32, u32, u32, Sender<ServiceResult<()>>),
    FocusPopup(u32),
    Restore(PasteTarget),
    Shutdown(Sender<()>),
}

#[derive(Clone)]
pub struct X11Service {
    tx: Sender<Command>,
    capabilities: Arc<Mutex<Capabilities>>,
}
impl X11Service {
    pub fn start(settings: Settings) -> Result<(Self, Receiver<PlatformEvent>)> {
        super::require_x11_session()?;
        let (tx, rx) = bounded(64);
        let (events_tx, events_rx) = bounded(64);
        let mut worker = Worker::connect(settings, events_tx)?;
        let capabilities = Arc::new(Mutex::new(worker.capabilities.clone()));
        worker.shared_capabilities = Some(capabilities.clone());
        thread::Builder::new()
            .name("feritto-x11".into())
            .spawn(move || {
                if let Err(error) = worker.run(rx) {
                    worker.emit(PlatformEvent::Status(format!(
                        "X11 service stopped: {error}"
                    )));
                    tracing::error!(%error,"X11 service stopped");
                }
            })?;
        Ok((Self { tx, capabilities }, events_rx))
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
            .map_err(|_| "X11 worker unavailable".to_string())?;
        rx.recv_timeout(Duration::from_secs(3))
            .map_err(|_| "X11 operation timed out".to_string())?
    }
    pub fn configure(&self, settings: Settings) {
        let _ = self.tx.send(Command::Settings(settings));
    }
    pub fn focus_popup(&self, window: u32) {
        let _ = self.tx.send(Command::FocusPopup(window));
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
}
impl ClipboardMonitor for X11Service {
    fn set_paused(&self, paused: bool) -> ServiceResult<()> {
        self.tx
            .send(Command::Pause(paused))
            .map_err(|e| e.to_string())
    }
}
impl ClipboardOwner for X11Service {
    fn publish(&self, offer: Arc<Offer>) -> ServiceResult<()> {
        self.request(|reply| Command::Publish(offer, None, reply))
    }
    fn clear(&self) -> ServiceResult<()> {
        self.request(Command::Clear)
    }
}
impl HotkeyService for X11Service {
    fn hotkey_capability(&self) -> Capability {
        self.capabilities().global_hotkey
    }
}
impl PasteInjector for X11Service {
    fn paste(
        &self,
        offer: Arc<Offer>,
        target: PasteTarget,
        shortcut: PasteShortcut,
    ) -> ServiceResult<()> {
        self.request(|reply| Command::Publish(offer, Some((target, shortcut)), reply))
    }
}
impl ActiveWindowTracker for X11Service {
    fn active_window(&self) -> ServiceResult<PasteTarget> {
        self.request(Command::Active)
    }
}
impl PopupPlacer for X11Service {
    fn prepare_popup(&self, window: u32, width: u32, height: u32) -> ServiceResult<()> {
        self.request(|reply| Command::Prepare(window, width, height, reply))
    }
}

#[derive(Clone)]
struct Published {
    data: HashMap<Atom, Arc<[u8]>>,
    timestamp: u32,
}
struct Outgoing {
    requestor: Window,
    property: Atom,
    target: Atom,
    bytes: Arc<[u8]>,
    offset: usize,
    progress: Instant,
}
struct Incoming {
    generation: u64,
    owner: Window,
    timestamp: u32,
    source: SourceIdentity,
    window: Window,
    property: Atom,
    target: Atom,
    stage: Stage,
    queue: VecDeque<Atom>,
    representations: Vec<Representation>,
    omitted: Vec<String>,
    bytes: Vec<u8>,
    total: usize,
    started: Instant,
    progress: Instant,
    secret: bool,
    save: Option<SelectionRequestEvent>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Targets,
    Secret,
    Data,
    Incremental,
}
struct Saved {
    generation: u64,
    owner: Window,
    offer: Arc<Offer>,
}
struct PendingPaste {
    target: PasteTarget,
    shortcut: PasteShortcut,
    deadline: Instant,
}

struct Worker {
    conn: RustConnection,
    root: Window,
    window: Window,
    atoms: Atoms,
    settings: Settings,
    events: Sender<PlatformEvent>,
    capabilities: Capabilities,
    shared_capabilities: Option<Arc<Mutex<Capabilities>>>,
    generation: u64,
    incoming: Option<Incoming>,
    saved: Option<Saved>,
    published: Option<Published>,
    outgoing: Vec<Outgoing>,
    paste: Option<PendingPaste>,
    owner: Window,
    timestamp: u32,
    manager: bool,
    popup: Window,
    hotkey: Option<(u8, Vec<ModMask>)>,
    contention: VecDeque<Instant>,
    chunk_size: usize,
}
impl Worker {
    fn connect(settings: Settings, events: Sender<PlatformEvent>) -> Result<Self> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?
        .check()?;
        let atoms = Atoms::new(&conn)?.reply()?;
        conn.xfixes_query_version(5, 0)?
            .reply()
            .context("XFixes is required for clipboard monitoring")?;
        conn.xfixes_select_selection_input(
            window,
            atoms.CLIPBOARD,
            xfixes::SelectionEventMask::SET_SELECTION_OWNER
                | xfixes::SelectionEventMask::SELECTION_WINDOW_DESTROY
                | xfixes::SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )?
        .check()?;
        let xtest = conn.extension_information("XTEST")?.is_some();
        let other = conn
            .get_selection_owner(atoms.CLIPBOARD_MANAGER)?
            .reply()?
            .owner;
        let manager = other == NONE && !settings.coexistence;
        if manager {
            conn.set_selection_owner(window, atoms.CLIPBOARD_MANAGER, CURRENT_TIME)?
                .check()?;
        }
        let capabilities = Capabilities {
            backend: "X11".into(),
            monitor: Capability::Available,
            ownership: Capability::Available,
            global_hotkey: Capability::Available,
            automatic_paste: if xtest {
                Capability::Available
            } else {
                Capability::Unavailable("XTest extension missing".into())
            },
            source_identity: Capability::Available,
            pointer_placement: Capability::Available,
            caret_placement: Capability::Unavailable(
                "Caret lookup is pending; using the pointer".into(),
            ),
            persistence: if manager {
                Capability::Available
            } else {
                Capability::Unavailable(
                    "Another clipboard manager or coexistence mode disables automatic recovery"
                        .into(),
                )
            },
        };
        let owner = conn.get_selection_owner(atoms.CLIPBOARD)?.reply()?.owner;
        let chunk_size = (conn.maximum_request_bytes().saturating_sub(128)).min(64 * 1024);
        let mut worker = Self {
            conn,
            root,
            window,
            atoms,
            settings,
            events,
            capabilities,
            shared_capabilities: None,
            generation: 0,
            incoming: None,
            saved: None,
            published: None,
            outgoing: vec![],
            paste: None,
            owner,
            timestamp: 0,
            manager,
            popup: NONE,
            hotkey: None,
            contention: VecDeque::new(),
            chunk_size,
        };
        worker.register_hotkey();
        if other != NONE {
            worker.emit(PlatformEvent::Status("Another clipboard manager detected; history capture remains active, automatic recovery is disabled".into()));
        }
        if owner != NONE {
            worker.begin_capture(owner, 0, None)?;
        }
        worker.conn.flush()?;
        Ok(worker)
    }
    fn emit(&self, event: PlatformEvent) {
        if self.events.try_send(event).is_err() {
            tracing::warn!("platform event queue full; an event could not be delivered");
        }
    }
    fn update_capabilities(&self) {
        if let Some(shared) = &self.shared_capabilities {
            *shared.lock().unwrap() = self.capabilities.clone();
        }
    }
    fn atom(&self, name: &str) -> Result<Atom> {
        Ok(self.conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
    }
    fn name(&self, atom: Atom) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.conn.get_atom_name(atom)?.reply()?.name).into_owned())
    }
    fn run(&mut self, rx: Receiver<Command>) -> Result<()> {
        loop {
            for _ in 0..256 {
                let Some(event) = self.conn.poll_for_event()? else {
                    break;
                };
                if let Err(error) = self.event(event) {
                    tracing::warn!(%error,"X11 operation failed");
                    self.emit(PlatformEvent::Status(format!(
                        "X11 operation failed: {error}"
                    )));
                }
            }
            self.timeouts()?;
            match rx.recv_timeout(Duration::from_millis(12)) {
                Ok(Command::Shutdown(reply)) => {
                    self.orderly_shutdown()?;
                    let _ = reply.send(());
                    return Ok(());
                }
                Ok(command) => self.command(command),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Ok(()),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
            self.conn.flush()?;
        }
    }
    fn command(&mut self, command: Command) {
        match command {
            Command::Publish(offer, paste, reply) => {
                let result = self.publish(
                    &offer,
                    paste.is_some_and(|(_, s)| matches!(s, PasteShortcut::ShiftInsert)),
                );
                let success = result.is_ok();
                let _ = reply.send(result.map_err(|e| e.to_string()));
                if success && let Some((target, shortcut)) = paste {
                    if let Err(error) = self.request_focus(target.window, target.timestamp) {
                        self.emit(PlatformEvent::PasteResult(format!(
                            "Copied; press your paste shortcut ({error})"
                        )));
                    } else {
                        self.paste = Some(PendingPaste {
                            target,
                            shortcut,
                            deadline: Instant::now() + Duration::from_millis(650),
                        });
                    }
                }
            }
            Command::Clear(reply) => {
                self.cancel_incoming();
                self.saved = None;
                self.published = None;
                let result = (|| -> Result<()> {
                    self.conn
                        .set_selection_owner(NONE, self.atoms.CLIPBOARD, CURRENT_TIME)?
                        .check()?;
                    Ok(())
                })();
                let _ = reply.send(result.map_err(|e| e.to_string()));
            }
            Command::Pause(paused) => {
                self.settings.paused = paused;
                if paused {
                    self.cancel_incoming();
                    self.saved = None;
                }
            }
            Command::Settings(settings) => {
                if settings.paused
                    || settings.capture.extra_targets != self.settings.capture.extra_targets
                {
                    self.cancel_incoming();
                    self.saved = None;
                }
                if settings.coexistence && self.manager {
                    let _ = self.conn.set_selection_owner(
                        NONE,
                        self.atoms.CLIPBOARD_MANAGER,
                        CURRENT_TIME,
                    );
                    self.manager = false;
                }
                self.settings = settings;
            }
            Command::Active(reply) => {
                let _ = reply.send(self.active().map_err(|e| e.to_string()));
            }
            Command::Prepare(window, w, h, reply) => {
                let _ = reply.send(self.prepare(window, w, h).map_err(|e| e.to_string()));
            }
            Command::FocusPopup(window) => {
                self.popup = window;
                let _ = self.request_focus(window, self.timestamp);
            }
            Command::Restore(target) => {
                let _ = self.request_focus(target.window, target.timestamp);
            }
            Command::Shutdown(_) => {}
        }
    }
    fn event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::XfixesSelectionNotify(e) if e.selection == self.atoms.CLIPBOARD => {
                self.owner_changed(e)?
            }
            Event::SelectionNotify(e) => self.selection_notify(e)?,
            Event::SelectionRequest(e) => self.serve(e)?,
            Event::SelectionClear(e) if e.selection == self.atoms.CLIPBOARD_MANAGER => {
                self.manager = false;
                self.capabilities.persistence =
                    Capability::Unavailable("Another manager owns CLIPBOARD_MANAGER".into());
                self.update_capabilities();
            }
            Event::PropertyNotify(e) => {
                if e.state == Property::NEW_VALUE {
                    self.incoming_chunk(e)?;
                } else {
                    self.outgoing_chunk(e.window, e.atom)?;
                }
            }
            Event::KeyPress(e) => {
                self.timestamp = e.time;
                if self
                    .hotkey
                    .as_ref()
                    .is_some_and(|(code, _)| *code == e.detail)
                {
                    self.emit(PlatformEvent::Toggle(self.active()?));
                }
            }
            Event::MappingNotify(_) => self.register_hotkey(),
            Event::Error(e) => tracing::debug!(error=?e,"asynchronous X11 error"),
            _ => {}
        }
        Ok(())
    }
    fn owner_changed(&mut self, e: xfixes::SelectionNotifyEvent) -> Result<()> {
        self.timestamp = e.timestamp;
        let previous = self.owner;
        self.owner = e.owner;
        if e.owner == self.window {
            return Ok(());
        }
        let destruction = e.subtype == xfixes::SelectionEvent::SELECTION_WINDOW_DESTROY
            || e.subtype == xfixes::SelectionEvent::SELECTION_CLIENT_CLOSE;
        if e.owner == NONE && destruction {
            self.cancel_incoming();
            if self.manager
                && !self.settings.coexistence
                && !self.settings.paused
                && let Some(saved) = self.saved.take()
                && saved.generation == self.generation
                && saved.owner == previous
            {
                // A server grab makes the owner check + timestamped claim atomic against a new copy.
                self.conn.grab_server()?.check()?;
                let unowned = self
                    .conn
                    .get_selection_owner(self.atoms.CLIPBOARD)?
                    .reply()?
                    .owner
                    == NONE;
                if unowned {
                    self.conn
                        .set_selection_owner(self.window, self.atoms.CLIPBOARD, e.timestamp)?
                        .check()?;
                }
                self.conn.ungrab_server()?.check()?;
                if unowned
                    && self
                        .conn
                        .get_selection_owner(self.atoms.CLIPBOARD)?
                        .reply()?
                        .owner
                        == self.window
                {
                    self.install_offer(&saved.offer, e.timestamp)?;
                    self.owner = self.window;
                }
            }
            self.saved = None;
            return Ok(());
        }
        // Explicit clears, new generations, and excluded offers invalidate all older recovery data.
        self.generation = self.generation.wrapping_add(1);
        self.saved = None;
        self.cancel_incoming();
        if e.owner == NONE {
            return Ok(());
        }
        if previous == self.window {
            let now = Instant::now();
            self.contention.push_back(now);
            while self
                .contention
                .front()
                .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(2))
            {
                self.contention.pop_front();
            }
            if self.contention.len() >= 3 {
                self.settings.coexistence = true;
                self.manager = false;
                if self
                    .conn
                    .get_selection_owner(self.atoms.CLIPBOARD_MANAGER)?
                    .reply()?
                    .owner
                    == self.window
                {
                    self.conn
                        .set_selection_owner(NONE, self.atoms.CLIPBOARD_MANAGER, e.timestamp)?
                        .check()?;
                }
                self.capabilities.persistence = Capability::Unavailable(
                    "Ownership contention detected; recovery disabled".into(),
                );
                self.update_capabilities();
                self.emit(PlatformEvent::Status(
                    "Clipboard ownership contention detected; switched to history-only capture"
                        .into(),
                ));
            }
        }
        self.begin_capture(e.owner, e.selection_timestamp, None)
    }
    fn cancel_incoming(&mut self) {
        if let Some(incoming) = self.incoming.take() {
            let _ = self.conn.destroy_window(incoming.window);
            if let Some(save) = incoming.save {
                let _ = self.notify(save, NONE);
            }
        }
    }
    fn source(&self, owner: Window) -> SourceIdentity {
        let application = self
            .conn
            .get_property(false, owner, self.atoms.WM_CLASS, AtomEnum::STRING, 0, 1024)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| {
                r.value
                    .split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .next_back()
                    .map(|s| String::from_utf8_lossy(s).into_owned())
            });
        SourceIdentity {
            application,
            window: Some(u64::from(owner)),
        }
    }
    fn begin_capture(
        &mut self,
        owner: Window,
        timestamp: u32,
        save: Option<SelectionRequestEvent>,
    ) -> Result<()> {
        if self.settings.paused {
            if let Some(save) = save {
                self.notify(save, NONE)?;
            }
            return Ok(());
        }
        let source = self.source(owner);
        if source.application.as_ref().is_some_and(|app| {
            self.settings
                .capture
                .excluded_applications
                .iter()
                .any(|excluded| app.eq_ignore_ascii_case(excluded))
        }) {
            if let Some(save) = save {
                self.notify(save, NONE)?;
            }
            return Ok(());
        }
        let window = self.conn.generate_id()?;
        self.conn
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )?
            .check()?;
        let property = self.atom("_FERITTO_CAPTURE")?;
        self.conn
            .convert_selection(
                window,
                self.atoms.CLIPBOARD,
                self.atoms.TARGETS,
                property,
                timestamp,
            )?
            .check()?;
        self.incoming = Some(Incoming {
            generation: self.generation,
            owner,
            timestamp,
            source,
            window,
            property,
            target: self.atoms.TARGETS,
            stage: Stage::Targets,
            queue: VecDeque::new(),
            representations: vec![],
            omitted: vec![],
            bytes: vec![],
            total: 0,
            started: Instant::now(),
            progress: Instant::now(),
            secret: false,
            save,
        });
        Ok(())
    }
    fn selection_notify(&mut self, e: SelectionNotifyEvent) -> Result<()> {
        let Some(mut input) = self.incoming.take() else {
            return Ok(());
        };
        if e.requestor != input.window
            || e.selection != self.atoms.CLIPBOARD
            || e.target != input.target
        {
            self.incoming = Some(input);
            return Ok(());
        }
        if self
            .conn
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner
            != input.owner
        {
            self.drop_capture(input);
            return Ok(());
        }
        if e.property == NONE {
            input.omitted.push(self.name(input.target)?);
            if input.stage == Stage::Targets || input.stage == Stage::Secret {
                self.drop_capture(input);
                return Ok(());
            }
            self.next_target(input)?;
            return Ok(());
        }
        let reply = self
            .conn
            .get_property(
                true,
                input.window,
                input.property,
                AtomEnum::ANY,
                0,
                (self.settings.capture.max_target_bytes / 4 + 1) as u32,
            )?
            .reply()?;
        input.progress = Instant::now();
        if input.stage == Stage::Targets {
            if reply.type_ != self.atoms.ATOM || reply.format != 32 || reply.bytes_after != 0 {
                self.drop_capture(input);
                return Ok(());
            }
            let targets: Vec<Atom> = reply.value32().into_iter().flatten().take(4096).collect();
            let mut names = Vec::new();
            for &atom in &targets {
                names.push(self.name(atom)?);
            }
            if targets.contains(&self.atoms.IGNORE) || targets.contains(&self.atoms.VIEWER_IGNORE) {
                self.drop_capture(input);
                return Ok(());
            }
            for name in self.settings.capture.select_targets(&names) {
                input.queue.push_back(self.atom(&name)?);
            }
            if let Some(save) = input.save
                && save.property != NONE
            {
                let requested = self
                    .conn
                    .get_property(
                        false,
                        save.requestor,
                        save.property,
                        self.atoms.ATOM,
                        0,
                        4096,
                    )?
                    .reply()?;
                if let Some(atoms) = requested.value32() {
                    let requested: Vec<_> = atoms.collect();
                    if !requested.is_empty() {
                        input.queue.retain(|atom| requested.contains(atom));
                    }
                }
            }
            if targets.contains(&self.atoms.KDE_SECRET) {
                input.target = self.atoms.KDE_SECRET;
                input.stage = Stage::Secret;
                self.conn
                    .convert_selection(
                        input.window,
                        self.atoms.CLIPBOARD,
                        input.target,
                        input.property,
                        input.timestamp,
                    )?
                    .check()?;
                self.incoming = Some(input);
            } else {
                self.next_target(input)?;
            }
        } else if reply.type_ == self.atoms.INCR {
            let announced = reply.value32().and_then(|mut v| v.next()).unwrap_or(0) as usize;
            if input.stage == Stage::Secret
                || announced > self.settings.capture.max_target_bytes
                || announced.saturating_add(input.total)
                    > self.settings.capture.max_generation_bytes
            {
                input.omitted.push(self.name(input.target)?);
                self.finish_capture(input)?;
            } else {
                input.stage = Stage::Incremental;
                input.bytes.clear();
                self.incoming = Some(input);
            }
        } else if input.stage == Stage::Secret {
            input.secret = reply.bytes_after != 0
                || reply.value.len() > 64
                || String::from_utf8_lossy(&reply.value)
                    .trim_end_matches('\0')
                    .trim()
                    .eq_ignore_ascii_case("secret");
            if input.secret {
                self.drop_capture(input);
            } else {
                self.next_target(input)?;
            }
        } else {
            if reply.bytes_after != 0
                || reply.value.len() > self.settings.capture.max_target_bytes
                || input.total.saturating_add(reply.value.len())
                    > self.settings.capture.max_generation_bytes
                || reply.format != 8
            {
                input.omitted.push(self.name(input.target)?);
            } else {
                self.add_representation(&mut input, reply.value)?;
            }
            self.next_target(input)?;
        }
        Ok(())
    }
    fn incoming_chunk(&mut self, e: PropertyNotifyEvent) -> Result<()> {
        let Some(input) = &self.incoming else {
            return Ok(());
        };
        if input.stage != Stage::Incremental || e.window != input.window || e.atom != input.property
        {
            return Ok(());
        }
        let mut input = self.incoming.take().unwrap();
        if self
            .conn
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner
            != input.owner
        {
            self.drop_capture(input);
            return Ok(());
        }
        let reply = self
            .conn
            .get_property(
                true,
                input.window,
                input.property,
                AtomEnum::ANY,
                0,
                (self.settings.capture.max_target_bytes / 4 + 1) as u32,
            )?
            .reply()?;
        // Ignore the stale PropertyNotify for the INCR header already consumed above.
        if reply.type_ == NONE {
            self.incoming = Some(input);
            return Ok(());
        }
        input.progress = Instant::now();
        if reply.bytes_after != 0
            || reply.format != 8
            || input.bytes.len().saturating_add(reply.value.len())
                > self.settings.capture.max_target_bytes
            || input
                .total
                .saturating_add(input.bytes.len())
                .saturating_add(reply.value.len())
                > self.settings.capture.max_generation_bytes
        {
            input.omitted.push(self.name(input.target)?);
            self.finish_capture(input)?;
        } else if reply.value.is_empty() {
            let bytes = std::mem::take(&mut input.bytes);
            self.add_representation(&mut input, bytes)?;
            self.next_target(input)?;
        } else {
            input.bytes.extend(reply.value);
            self.incoming = Some(input);
        }
        Ok(())
    }
    fn add_representation(&self, input: &mut Incoming, bytes: Vec<u8>) -> Result<()> {
        let target = self.name(input.target)?;
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
        Ok(())
    }
    fn next_target(&mut self, mut input: Incoming) -> Result<()> {
        if let Some(target) = input.queue.pop_front() {
            input.target = target;
            input.stage = Stage::Data;
            input.bytes.clear();
            input.progress = Instant::now();
            self.conn
                .convert_selection(
                    input.window,
                    self.atoms.CLIPBOARD,
                    target,
                    input.property,
                    input.timestamp,
                )?
                .check()?;
            self.incoming = Some(input);
            Ok(())
        } else {
            self.finish_capture(input)
        }
    }
    fn drop_capture(&self, input: Incoming) {
        let _ = self.conn.destroy_window(input.window);
        if let Some(save) = input.save {
            let _ = self.notify(save, NONE);
        }
    }
    fn finish_capture(&mut self, input: Incoming) -> Result<()> {
        let _ = self.conn.destroy_window(input.window);
        if input.representations.is_empty() {
            if let Some(save) = input.save {
                self.notify(save, NONE)?;
            }
            return Ok(());
        }
        let complete = input.omitted.is_empty() && input.queue.is_empty();
        let offer = Arc::new(Offer {
            representations: input.representations,
        });
        let owner = self
            .conn
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner;
        if input.generation != self.generation || owner != input.owner {
            if let Some(save) = input.save {
                self.notify(save, NONE)?;
            }
            return Ok(());
        }
        if complete {
            self.saved = Some(Saved {
                generation: input.generation,
                owner: input.owner,
                offer: offer.clone(),
            });
        }
        if let Some(save) = input.save {
            if complete {
                self.publish(&offer, false)?;
                let targets: Vec<_> = offer
                    .representations
                    .iter()
                    .filter_map(|r| self.atom(&r.target).ok())
                    .collect();
                let property = if save.property == NONE {
                    save.target
                } else {
                    save.property
                };
                self.conn.change_property32(
                    PropMode::REPLACE,
                    save.requestor,
                    property,
                    self.atoms.ATOM,
                    &targets,
                )?;
                self.notify(save, property)?;
            } else {
                self.notify(save, NONE)?;
            }
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
        Ok(())
    }
    fn server_time(&mut self) -> Result<u32> {
        self.conn.change_property8(
            PropMode::REPLACE,
            self.window,
            self.atoms._FERITTO_TIME,
            self.atoms.INTEGER,
            &[0],
        )?;
        self.conn.flush()?;
        loop {
            let event = self.conn.wait_for_event()?;
            if let Event::PropertyNotify(e) = &event
                && e.window == self.window
                && e.atom == self.atoms._FERITTO_TIME
            {
                self.timestamp = e.time;
                return Ok(e.time);
            }
            self.event(event)?;
        }
    }
    fn install_offer(&mut self, offer: &Offer, timestamp: u32) -> Result<()> {
        let mut data = HashMap::new();
        for r in &offer.representations {
            if !is_protocol_target(&r.target) {
                data.insert(self.atom(&r.target)?, r.bytes.clone());
            }
        }
        if let Some(text) = offer.text_content() {
            let utf8: Arc<[u8]> = Arc::from(text.as_bytes());
            for target in [
                "UTF8_STRING",
                "text/plain;charset=utf-8",
                "text/plain;charset=UTF-8",
                "text/plain",
                "TEXT",
            ] {
                data.insert(self.atom(target)?, utf8.clone());
            }
            if text.chars().all(|c| u32::from(c) <= 255) {
                data.insert(
                    self.atoms.STRING,
                    Arc::from(text.chars().map(|c| c as u8).collect::<Vec<_>>()),
                );
            }
        }
        if data.is_empty() {
            bail!("no supported formats in clipboard offer")
        }
        self.published = Some(Published { data, timestamp });
        Ok(())
    }
    fn publish(&mut self, offer: &Offer, primary: bool) -> Result<()> {
        let timestamp = self.server_time()?;
        self.install_offer(offer, timestamp)?;
        self.conn
            .set_selection_owner(self.window, self.atoms.CLIPBOARD, timestamp)?
            .check()?;
        if primary {
            self.conn
                .set_selection_owner(self.window, AtomEnum::PRIMARY.into(), timestamp)?
                .check()?;
        }
        if self
            .conn
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner
            != self.window
        {
            bail!("clipboard ownership was refused")
        }
        self.owner = self.window;
        self.saved = None;
        self.cancel_incoming();
        Ok(())
    }
    fn notify(&self, request: SelectionRequestEvent, property: Atom) -> Result<()> {
        self.conn
            .send_event(
                false,
                request.requestor,
                EventMask::NO_EVENT,
                SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: request.time,
                    requestor: request.requestor,
                    selection: request.selection,
                    target: request.target,
                    property,
                },
            )?
            .check()?;
        Ok(())
    }
    fn serve(&mut self, e: SelectionRequestEvent) -> Result<()> {
        if e.selection == self.atoms.CLIPBOARD_MANAGER
            && self.manager
            && e.target == self.atoms.SAVE_TARGETS
        {
            let owner = self
                .conn
                .get_selection_owner(self.atoms.CLIPBOARD)?
                .reply()?
                .owner;
            if owner == NONE || owner == self.window {
                self.notify(e, NONE)?;
            } else {
                self.cancel_incoming();
                self.begin_capture(owner, e.time, Some(e))?;
            }
            return Ok(());
        }
        if e.selection != self.atoms.CLIPBOARD && e.selection != u32::from(AtomEnum::PRIMARY) {
            self.notify(e, NONE)?;
            return Ok(());
        }
        if self.conn.get_selection_owner(e.selection)?.reply()?.owner != self.window {
            self.notify(e, NONE)?;
            return Ok(());
        }
        let Some(published) = self.published.clone() else {
            self.notify(e, NONE)?;
            return Ok(());
        };
        if e.time != CURRENT_TIME && (e.time.wrapping_sub(published.timestamp) as i32) < 0 {
            self.notify(e, NONE)?;
            return Ok(());
        }
        let property = if e.property == NONE {
            e.target
        } else {
            e.property
        };
        if e.target == self.atoms.MULTIPLE {
            if e.property == NONE {
                self.notify(e, NONE)?;
                return Ok(());
            }
            let reply = self
                .conn
                .get_property(false, e.requestor, property, AtomEnum::ANY, 0, 8192)?
                .reply()?;
            if reply.format != 32
                || reply.bytes_after != 0
                || (reply.type_ != self.atoms.ATOM_PAIR && reply.type_ != self.atoms.ATOM)
            {
                self.notify(e, NONE)?;
                return Ok(());
            }
            let mut pairs: Vec<_> = reply.value32().into_iter().flatten().collect();
            if pairs.len() % 2 != 0 {
                self.notify(e, NONE)?;
                return Ok(());
            }
            let mut used = Vec::new();
            for pair in pairs.chunks_exact_mut(2) {
                if pair[0] == self.atoms.MULTIPLE
                    || pair[1] == NONE
                    || pair[1] == property
                    || used.contains(&pair[1])
                    || !self.convert(e.requestor, pair[0], pair[1], &published)?
                {
                    pair[1] = NONE;
                } else {
                    used.push(pair[1]);
                }
            }
            self.conn.change_property32(
                PropMode::REPLACE,
                e.requestor,
                property,
                self.atoms.ATOM_PAIR,
                &pairs,
            )?;
            self.notify(e, property)?;
        } else {
            let ok = self.convert(e.requestor, e.target, property, &published)?;
            self.notify(e, if ok { property } else { NONE })?;
        }
        Ok(())
    }
    fn convert(
        &mut self,
        requestor: Window,
        target: Atom,
        property: Atom,
        published: &Published,
    ) -> Result<bool> {
        if target == self.atoms.TARGETS {
            let mut targets: Vec<_> = published.data.keys().copied().collect();
            targets.extend([
                self.atoms.TARGETS,
                self.atoms.TIMESTAMP,
                self.atoms.MULTIPLE,
            ]);
            targets.sort_unstable();
            targets.dedup();
            self.conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                self.atoms.ATOM,
                &targets,
            )?;
        } else if target == self.atoms.TIMESTAMP {
            self.conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                self.atoms.INTEGER,
                &[published.timestamp],
            )?;
        } else if let Some(bytes) = published.data.get(&target) {
            if bytes.len() <= self.chunk_size {
                self.conn.change_property8(
                    PropMode::REPLACE,
                    requestor,
                    property,
                    target,
                    bytes,
                )?;
            } else {
                if self.outgoing.len() >= 32
                    || self
                        .outgoing
                        .iter()
                        .any(|o| o.requestor == requestor && o.property == property)
                {
                    return Ok(false);
                }
                self.conn
                    .change_window_attributes(
                        requestor,
                        &ChangeWindowAttributesAux::new()
                            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
                    )?
                    .check()?;
                self.conn.change_property32(
                    PropMode::REPLACE,
                    requestor,
                    property,
                    self.atoms.INCR,
                    &[bytes.len() as u32],
                )?;
                self.outgoing.push(Outgoing {
                    requestor,
                    property,
                    target,
                    bytes: bytes.clone(),
                    offset: 0,
                    progress: Instant::now(),
                });
            }
        } else {
            return Ok(false);
        }
        Ok(true)
    }
    fn outgoing_chunk(&mut self, requestor: Window, property: Atom) -> Result<()> {
        let Some(index) = self
            .outgoing
            .iter()
            .position(|o| o.requestor == requestor && o.property == property)
        else {
            return Ok(());
        };
        let output = &mut self.outgoing[index];
        let end = (output.offset + self.chunk_size).min(output.bytes.len());
        self.conn.change_property8(
            PropMode::REPLACE,
            requestor,
            property,
            output.target,
            &output.bytes[output.offset..end],
        )?;
        output.progress = Instant::now();
        if output.offset == output.bytes.len() {
            self.outgoing.swap_remove(index);
        } else {
            output.offset = end;
        }
        Ok(())
    }
    fn register_hotkey(&mut self) {
        if let Some((code, mods)) = self.hotkey.take() {
            for modifier in mods {
                let _ = self.conn.ungrab_key(code, self.root, modifier);
            }
        }
        let result = (|| -> Result<(u8, Vec<ModMask>)> {
            let min = self.conn.setup().min_keycode;
            let count = self.conn.setup().max_keycode - min + 1;
            let mapping = self.conn.get_keyboard_mapping(min, count)?.reply()?;
            let code = mapping
                .keysyms
                .chunks(mapping.keysyms_per_keycode as usize)
                .position(|syms| syms.contains(&0x60))
                .map(|i| min + i as u8)
                .context("grave key is absent from the keyboard map")?;
            let modifiers = self.conn.get_modifier_mapping()?.reply()?;
            let per = modifiers.keycodes.len() / 8;
            let mut num = ModMask::default();
            for (index, codes) in modifiers.keycodes.chunks(per.max(1)).enumerate() {
                for &key in codes {
                    if key >= min && key <= self.conn.setup().max_keycode {
                        let start = (key - min) as usize * mapping.keysyms_per_keycode as usize;
                        if mapping.keysyms[start..start + mapping.keysyms_per_keycode as usize]
                            .contains(&0xff7f)
                        {
                            num = ModMask::from(1u16 << index);
                        }
                    }
                }
            }
            let mut variants = vec![
                ModMask::CONTROL,
                ModMask::CONTROL | ModMask::LOCK,
                ModMask::CONTROL | num,
                ModMask::CONTROL | num | ModMask::LOCK,
            ];
            variants.sort_by_key(|m| u16::from(*m));
            variants.dedup();
            for &variant in &variants {
                if let Err(error) = self
                    .conn
                    .grab_key(
                        false,
                        self.root,
                        variant,
                        code,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                    )?
                    .check()
                {
                    for &undo in &variants {
                        let _ = self.conn.ungrab_key(code, self.root, undo);
                    }
                    bail!(
                        "Ctrl+grave is already registered ({error}); bind feritto --toggle in your desktop instead"
                    );
                }
            }
            Ok((code, variants))
        })();
        match result {
            Ok(hotkey) => {
                self.hotkey = Some(hotkey);
                self.capabilities.global_hotkey = Capability::Available;
            }
            Err(error) => {
                self.capabilities.global_hotkey = Capability::Unavailable(error.to_string());
                self.emit(PlatformEvent::Status(error.to_string()));
            }
        }
        self.update_capabilities();
    }
    fn active(&self) -> Result<PasteTarget> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms._NET_ACTIVE_WINDOW,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?;
        let mut window = reply.value32().and_then(|mut v| v.next()).unwrap_or(NONE);
        if window == NONE {
            window = self.conn.get_input_focus()?.reply()?.focus;
        }
        if window <= 1 || window == self.root || window == self.window || window == self.popup {
            window = NONE;
        }
        Ok(PasteTarget {
            window,
            timestamp: self.timestamp,
        })
    }
    fn prepare(&mut self, window: Window, width: u32, height: u32) -> Result<()> {
        self.popup = window;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                window,
                self.atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                &[self.atoms._NET_WM_WINDOW_TYPE_UTILITY],
            )?
            .check()?;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                window,
                self.atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                &[
                    self.atoms._NET_WM_STATE_SKIP_TASKBAR,
                    self.atoms._NET_WM_STATE_SKIP_PAGER,
                    self.atoms._NET_WM_STATE_ABOVE,
                ],
            )?
            .check()?;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                window,
                self.atoms._NET_WM_USER_TIME,
                AtomEnum::CARDINAL,
                &[self.timestamp],
            )?
            .check()?;
        // Mark as transient so tiling WMs float the managed popup.
        self.conn
            .change_property32(
                PropMode::REPLACE,
                window,
                self.atoms.WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                &[self.root],
            )?
            .check()?;
        let pointer = self.conn.query_pointer(self.root)?.reply()?;
        let screen = &self
            .conn
            .setup()
            .roots
            .iter()
            .find(|s| s.root == self.root)
            .context("X11 screen disappeared")?;
        let (mut left, mut top, mut right, mut bottom) = (
            0i32,
            0i32,
            i32::from(screen.width_in_pixels),
            i32::from(screen.height_in_pixels),
        );
        use x11rb::protocol::randr::ConnectionExt as _;
        if let Ok(cookie) = self.conn.randr_get_monitors(self.root, true)
            && let Ok(monitors) = cookie.reply()
            && let Some(m) = monitors.monitors.iter().find(|m| {
                i32::from(pointer.root_x) >= i32::from(m.x)
                    && i32::from(pointer.root_x) < i32::from(m.x) + i32::from(m.width)
                    && i32::from(pointer.root_y) >= i32::from(m.y)
                    && i32::from(pointer.root_y) < i32::from(m.y) + i32::from(m.height)
            })
        {
            left = i32::from(m.x);
            top = i32::from(m.y);
            right = left + i32::from(m.width);
            bottom = top + i32::from(m.height);
        }
        let work = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms._NET_WORKAREA,
                AtomEnum::CARDINAL,
                0,
                4096,
            )?
            .reply()?;
        let desktop = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms._NET_CURRENT_DESKTOP,
                AtomEnum::CARDINAL,
                0,
                1,
            )?
            .reply()?
            .value32()
            .and_then(|mut v| v.next())
            .unwrap_or(0) as usize;
        if let Some(values) = work.value32() {
            let values: Vec<_> = values.collect();
            if let Some(area) = values.get(desktop * 4..desktop * 4 + 4) {
                left = left.max(area[0] as i32);
                top = top.max(area[1] as i32);
                right = right.min(area[0].saturating_add(area[2]) as i32);
                bottom = bottom.min(area[1].saturating_add(area[3]) as i32);
            }
        }
        let x = (i32::from(pointer.root_x) + 12).clamp(left, (right - width as i32).max(left));
        let y = (i32::from(pointer.root_y) + 12).clamp(top, (bottom - height as i32).max(top));
        self.conn
            .configure_window(window, &ConfigureWindowAux::new().x(x).y(y))?
            .check()?;
        self.conn.get_input_focus()?.reply()?; // Cross-connection round trip before iced maps the window.
        Ok(())
    }
    fn request_focus(&self, window: Window, timestamp: u32) -> Result<()> {
        if window <= 1 || window == self.root {
            bail!("the previous window is unavailable")
        }
        let attrs = self.conn.get_window_attributes(window)?.reply()?;
        if attrs.map_state != MapState::VIEWABLE {
            bail!("the previous window is no longer visible")
        }
        let active = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms._NET_ACTIVE_WINDOW,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?
            .value32()
            .and_then(|mut v| v.next())
            .unwrap_or(NONE);
        let event = ClientMessageEvent::new(
            32,
            window,
            self.atoms._NET_ACTIVE_WINDOW,
            [1, timestamp, active, 0, 0],
        );
        self.conn
            .send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )?
            .check()?;
        // No WM (e.g. protocol tests): focus only an existing, mapped target explicitly selected by the user.
        if self
            .conn
            .get_property(
                false,
                self.root,
                self.atom("_NET_SUPPORTING_WM_CHECK")?,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?
            .value
            .is_empty()
        {
            self.conn
                .set_input_focus(InputFocus::PARENT, window, timestamp)?
                .check()?;
        }
        Ok(())
    }
    fn focus_is_within(&self, target: Window) -> bool {
        let Ok(cookie) = self.conn.get_input_focus() else {
            return false;
        };
        let Ok(focus) = cookie.reply() else {
            return false;
        };
        let mut window = focus.focus;
        for _ in 0..32 {
            if window == target {
                return true;
            }
            if window <= 1 || window == self.root {
                break;
            }
            let Ok(cookie) = self.conn.query_tree(window) else {
                break;
            };
            let Ok(tree) = cookie.reply() else { break };
            window = tree.parent;
        }
        false
    }
    fn inject(&self, shortcut: PasteShortcut) -> Result<()> {
        if !matches!(self.capabilities.automatic_paste, Capability::Available) {
            bail!("automatic paste unavailable")
        }
        use x11rb::protocol::xkb::ConnectionExt as _;
        let group = self
            .conn
            .xkb_get_state(0x100u16)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|s| u8::from(s.group) as usize)
            .unwrap_or(0);
        let min = self.conn.setup().min_keycode;
        let count = self.conn.setup().max_keycode - min + 1;
        let mapping = self.conn.get_keyboard_mapping(min, count)?.reply()?;
        let resolve = |symbol: u32| -> Result<u8> {
            mapping
                .keysyms
                .chunks(mapping.keysyms_per_keycode as usize)
                .position(|syms| {
                    let offset = group * 2;
                    syms.get(offset).copied() == Some(symbol)
                        || (group == 0 && syms.first().copied() == Some(symbol))
                })
                .map(|i| min + i as u8)
                .context("paste keys are unavailable in the active layout; use your paste shortcut")
        };
        let shift = resolve(0xffe1)?;
        let control = resolve(0xffe3)?;
        let (key, mods) = match shortcut {
            PasteShortcut::ControlV => (resolve(0x76)?, vec![control]),
            PasteShortcut::ControlShiftV => (resolve(0x76)?, vec![control, shift]),
            PasteShortcut::ShiftInsert => (resolve(0xff63)?, vec![shift]),
        };
        for &code in &mods {
            self.conn
                .xtest_fake_input(KEY_PRESS_EVENT, code, CURRENT_TIME, self.root, 0, 0, 0)?
                .check()?;
        }
        self.conn
            .xtest_fake_input(KEY_PRESS_EVENT, key, CURRENT_TIME, self.root, 0, 0, 0)?
            .check()?;
        self.conn
            .xtest_fake_input(KEY_RELEASE_EVENT, key, CURRENT_TIME, self.root, 0, 0, 0)?
            .check()?;
        for &code in mods.iter().rev() {
            self.conn
                .xtest_fake_input(KEY_RELEASE_EVENT, code, CURRENT_TIME, self.root, 0, 0, 0)?
                .check()?;
        }
        Ok(())
    }
    fn timeouts(&mut self) -> Result<()> {
        let now = Instant::now();
        if self.incoming.as_ref().is_some_and(|input| {
            now.duration_since(input.progress)
                > Duration::from_millis(self.settings.capture.progress_timeout_ms)
                || now.duration_since(input.started)
                    > Duration::from_millis(self.settings.capture.generation_timeout_ms)
        }) {
            let mut input = self.incoming.take().unwrap();
            input.omitted.push(self.name(input.target)?);
            self.finish_capture(input)?;
        }
        self.outgoing
            .retain(|out| now.duration_since(out.progress) < Duration::from_secs(10));
        if let Some(paste) = self.paste.take() {
            let valid = self
                .conn
                .get_selection_owner(self.atoms.CLIPBOARD)?
                .reply()?
                .owner
                == self.window;
            let modifiers = self.conn.query_pointer(self.root)?.reply()?.mask;
            let held = u16::from(modifiers)
                & u16::from(
                    KeyButMask::SHIFT
                        | KeyButMask::CONTROL
                        | KeyButMask::MOD1
                        | KeyButMask::MOD4
                        | KeyButMask::MOD5,
                )
                != 0;
            if !valid {
                self.emit(PlatformEvent::PasteResult(
                    "Clipboard changed before paste; no keys were sent".into(),
                ));
            } else if self.focus_is_within(paste.target.window) && !held {
                // Hold the server only for the final focus check and XTest requests, never while waiting.
                self.conn.grab_server()?.check()?;
                let result = if self.focus_is_within(paste.target.window) {
                    self.inject(paste.shortcut)
                } else {
                    Err(anyhow::anyhow!("focus changed"))
                };
                self.conn.ungrab_server()?.check()?;
                self.emit(PlatformEvent::PasteResult(match result {
                    Ok(()) => "Pasted into the previous window".into(),
                    Err(error) => format!("Copied; press your paste shortcut ({error})"),
                }));
            } else if now >= paste.deadline {
                self.emit(PlatformEvent::PasteResult("Copied; press your paste shortcut (focus or modifier release could not be confirmed)".into()));
            } else {
                self.paste = Some(paste);
            }
        }
        Ok(())
    }
    fn orderly_shutdown(&mut self) -> Result<()> {
        if self.settings.clear_clipboard_on_exit {
            self.conn
                .set_selection_owner(NONE, self.atoms.CLIPBOARD, CURRENT_TIME)?
                .check()?;
        } else if self
            .conn
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner
            == self.window
        {
            let manager = self
                .conn
                .get_selection_owner(self.atoms.CLIPBOARD_MANAGER)?
                .reply()?
                .owner;
            if manager != NONE && manager != self.window {
                let property = self.atom("_FERITTO_HANDOFF")?;
                self.conn.convert_selection(
                    self.window,
                    self.atoms.CLIPBOARD_MANAGER,
                    self.atoms.SAVE_TARGETS,
                    property,
                    CURRENT_TIME,
                )?;
                self.conn.flush()?;
                let deadline = Instant::now() + Duration::from_millis(700);
                while Instant::now() < deadline {
                    if let Some(event) = self.conn.poll_for_event()? {
                        if matches!(&event,Event::SelectionNotify(e) if e.selection==self.atoms.CLIPBOARD_MANAGER)
                        {
                            break;
                        }
                        self.event(event)?;
                    } else {
                        thread::sleep(Duration::from_millis(5));
                    }
                    self.conn.flush()?;
                }
            }
        }
        self.conn.flush()?;
        Ok(())
    }
}
