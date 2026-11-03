//! Shared iced state and views. Only the window runner acquires native handles.
use feritto_core::*;
use feritto_platform::X11Service;
use feritto_store::Store;
use iced::{
    Element, Event,
    Length::{self, Fill},
    Size, Subscription, Task, Theme,
    keyboard::{self, Key, key::Named},
    widget::{
        self, button, checkbox, column, container, image, row, scrollable, space, text, text_input,
    },
    window,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub enum Control {
    Show,
    Toggle(PasteTarget),
    Hide,
    Settings,
    Quit,
    Pause,
    HistoryChanged,
    Status(String),
}

#[derive(Clone)]
pub struct Services {
    pub store: Store,
    pub platform: X11Service,
    pub controls: crossbeam_channel::Receiver<Control>,
    pub settings: Arc<Mutex<Settings>>,
    pub settings_path: PathBuf,
}

const WIDTH: f32 = 720.0;
const HEIGHT: f32 = 660.0;
const ROW: f32 = 62.0;
const VIEWPORT: f32 = 310.0;
const OVERSCAN: usize = 4;
const SEARCH_ID: &str = "search";
const LIST_ID: &str = "history";
#[derive(Debug, Clone)]
struct Page {
    generation: u64,
    start: usize,
    summaries: Vec<ClipSummary>,
    thumbnails: Vec<(ClipId, image::Handle)>,
}
#[derive(Debug, Clone)]
enum Message {
    Control(Control),
    SearchDue(u64),
    Event(window::Id, Event, iced::event::Status),
    Opened(window::Id),
    Native(Result<u32, String>),
    Target(Result<PasteTarget, String>),
    Prepared(Result<(), String>),
    Mapped,
    Search(String),
    SearchReady(u64, Result<Arc<Vec<ClipId>>, String>),
    PageReady(Result<Page, String>),
    Scroll(f32),
    Select(ClipId),
    SearchFocused(bool),
    Accept(bool),
    Published(Result<(), String>),
    Settings,
    Pause(bool),
    AutoPaste(bool),
    Dark(bool),
    Coexist(bool),
    Text(bool),
    Images(bool),
    Html(bool),
    Rtf(bool),
    Files(bool),
    Shortcut(PasteShortcut),
    MaxClips(String),
    Age(String),
    Star,
    Delete,
    ClearHistory,
    ConfirmClearHistory,
    ClearClipboard,
    Done(Result<(), String>),
    Quit,
    Exit,
}

struct App {
    services: Services,
    popup: window::Id,
    settings_window: Option<window::Id>,
    xid: Option<u32>,
    visible: bool,
    pending_show: bool,
    mapped_at: Instant,
    target: PasteTarget,
    query: String,
    generation: u64,
    ids: Arc<Vec<ClipId>>,
    selected: Option<ClipId>,
    last_click: Option<(ClipId, Instant)>,
    offset: f32,
    page_start: usize,
    page_pending: Option<(u64, usize)>,
    summaries: HashMap<ClipId, ClipSummary>,
    thumbnails: HashMap<ClipId, image::Handle>,
    status: String,
    settings: Settings,
    search_focused: bool,
    composing: bool,
    confirm_clear: bool,
}

struct ControlStream(crossbeam_channel::Receiver<Control>);
impl std::hash::Hash for ControlStream {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::any::TypeId::of::<Self>().hash(state);
    }
}

pub fn run(services: Services, show: bool) -> iced::Result {
    iced::daemon(
        move || App::boot(services.clone(), show),
        App::update,
        App::view,
    )
    .title(|app: &App, id| {
        if Some(id) == app.settings_window {
            "feritto — Settings".into()
        } else {
            "feritto".into()
        }
    })
    .theme(|app: &App, _| {
        if app.settings.dark_theme {
            Theme::Dark
        } else {
            Theme::Light
        }
    })
    .subscription(App::subscription)
    .run()
}

async fn background<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| e.to_string())?
}
impl App {
    fn boot(services: Services, show: bool) -> (Self, Task<Message>) {
        let settings = services.settings.lock().unwrap().clone();
        let (popup, task) = window::open(window::Settings {
            size: Size::new(WIDTH, HEIGHT),
            visible: false,
            resizable: false,
            decorations: false,
            exit_on_close_request: false,
            platform_specific: window::settings::PlatformSpecific {
                application_id: "io.github.gh2302.Feritto".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut app = Self {
            services,
            popup,
            settings_window: None,
            xid: None,
            visible: false,
            pending_show: show,
            mapped_at: Instant::now(),
            target: PasteTarget::default(),
            query: String::new(),
            generation: 0,
            ids: Arc::new(vec![]),
            selected: None,
            last_click: None,
            offset: 0.0,
            page_start: usize::MAX,
            page_pending: None,
            summaries: HashMap::new(),
            thumbnails: HashMap::new(),
            status: "Copy something to start your history · Ctrl+` opens feritto".into(),
            settings,
            search_focused: true,
            composing: false,
            confirm_clear: false,
        };
        let search = app.search();
        (app, Task::batch([task.map(Message::Opened), search]))
    }
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run_with(ControlStream(self.services.controls.clone()), |input| {
                let receiver = input.0.clone();
                iced::stream::channel::<Message>(
                    64,
                    move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                        use iced::futures::SinkExt;
                        loop {
                            let rx = receiver.clone();
                            match tokio::task::spawn_blocking(move || {
                                rx.recv_timeout(Duration::from_millis(250))
                            })
                            .await
                            {
                                Ok(Ok(control)) => {
                                    if output.send(Message::Control(control)).await.is_err() {
                                        break;
                                    }
                                }
                                Ok(Err(crossbeam_channel::RecvTimeoutError::Timeout)) => {}
                                _ => break,
                            }
                        }
                    },
                )
            }),
            iced::event::listen_with(|event, status, id| Some(Message::Event(id, event, status))),
        ])
    }
    fn search(&mut self) -> Task<Message> {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let query = self.query.clone();
        let store = self.services.store.clone();
        self.page_start = usize::MAX;
        self.page_pending = None;
        Task::perform(
            background(move || {
                store
                    .call(move |repo| repo.search_ids(&query))
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            }),
            move |r| Message::SearchReady(generation, r),
        )
    }
    fn load_page(&mut self) -> Task<Message> {
        let start = ((self.offset / ROW).floor() as usize)
            .saturating_sub(OVERSCAN)
            .min(self.ids.len());
        if self.page_start == start || self.page_pending == Some((self.generation, start)) {
            return Task::none();
        }
        self.page_pending = Some((self.generation, start));
        let generation = self.generation;
        let end = (start + (VIEWPORT / ROW).ceil() as usize + OVERSCAN * 2).min(self.ids.len());
        let ids = self.ids[start..end].to_vec();
        let store = self.services.store.clone();
        Task::perform(
            background(move || {
                store
                    .call(move |repo| {
                        let summaries = repo.summaries(&ids)?;
                        let mut thumbnails = Vec::new();
                        for id in ids {
                            if let Some(t) = repo.thumbnail(id)? {
                                thumbnails.push((
                                    id,
                                    image::Handle::from_rgba(t.width, t.height, t.rgba),
                                ));
                            }
                        }
                        Ok(Page {
                            generation,
                            start,
                            summaries,
                            thumbnails,
                        })
                    })
                    .map_err(|e| e.to_string())
            }),
            Message::PageReady,
        )
    }
    fn show(&mut self, target: Option<PasteTarget>) -> Task<Message> {
        if self.xid.is_none() {
            self.pending_show = true;
            return Task::none();
        }
        if let Some(target) = target
            && target.window != 0
        {
            self.target = target;
        }
        self.query.clear();
        self.last_click = None;
        self.offset = 0.0;
        self.selected = None;
        self.pending_show = false;
        let search = self.search();
        let platform = self.services.platform.clone();
        if target.is_none() {
            return Task::batch([
                search,
                Task::perform(
                    background(move || platform.active_window()),
                    Message::Target,
                ),
            ]);
        }
        Task::batch([search, self.prepare()])
    }
    fn prepare(&self) -> Task<Message> {
        let Some(xid) = self.xid else {
            return Task::none();
        };
        let platform = self.services.platform.clone();
        // Read physical dimensions from the server in the platform adapter; these are fallback sizes.
        Task::perform(
            background(move || platform.prepare_popup(xid, WIDTH as u32, HEIGHT as u32)),
            Message::Prepared,
        )
    }
    fn hide(&mut self, restore: bool) -> Task<Message> {
        self.visible = false;
        self.pending_show = false;
        let task = window::set_mode(self.popup, window::Mode::Hidden);
        if restore {
            let platform = self.services.platform.clone();
            let target = self.target;
            task.chain(Task::perform(
                background(move || {
                    platform.restore(target);
                    Ok(())
                }),
                Message::Done,
            ))
        } else {
            task
        }
    }
    fn persist(&mut self) -> Task<Message> {
        *self.services.settings.lock().unwrap() = self.settings.clone();
        self.services.platform.configure(self.settings.clone());
        let settings = self.settings.clone();
        let path = self.services.settings_path.clone();
        let store = self.services.store.clone();
        Task::perform(
            background(move || {
                let bytes = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
                feritto_store::write_private(&path, &bytes).map_err(|e| e.to_string())?;
                store
                    .call(move |repo| repo.cleanup(&settings))
                    .map_err(|e| e.to_string())
            }),
            Message::Done,
        )
    }
    fn navigate(&mut self, delta: isize) -> Task<Message> {
        if self.ids.is_empty() {
            return Task::none();
        }
        let current = self
            .selected
            .and_then(|id| self.ids.iter().position(|i| *i == id))
            .unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(self.ids.len() - 1);
        self.selected = Some(self.ids[next]);
        self.search_focused = false;
        let y = next as f32 * ROW;
        if y < self.offset {
            self.offset = y;
        } else if y + ROW > self.offset + VIEWPORT {
            self.offset = y + ROW - VIEWPORT;
        }
        Task::batch([
            self.load_page(),
            widget::operation::focus("list-focus"),
            widget::operation::scroll_to(
                LIST_ID,
                scrollable::AbsoluteOffset {
                    x: 0.0,
                    y: self.offset,
                },
            ),
        ])
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Control(control) => match control {
                Control::Show => self.show(None),
                Control::Toggle(target) => {
                    if self.visible {
                        self.hide(true)
                    } else {
                        self.show(Some(target))
                    }
                }
                Control::Hide => self.hide(true),
                Control::Settings => self.update(Message::Settings),
                Control::Quit => self.update(Message::Quit),
                Control::Pause => self.update(Message::Pause(!self.settings.paused)),
                Control::HistoryChanged => {
                    if self.visible {
                        self.search()
                    } else {
                        Task::none()
                    }
                }
                Control::Status(status) => {
                    self.status = status;
                    Task::none()
                }
            },
            Message::SearchDue(generation) => {
                if generation == self.generation {
                    self.search()
                } else {
                    Task::none()
                }
            }
            Message::Opened(id) if id == self.popup => window::run(id, |handle| {
                use window::raw_window_handle::RawWindowHandle;
                match handle.window_handle().map_err(|e| e.to_string())?.as_raw() {
                    RawWindowHandle::Xlib(h) => {
                        u32::try_from(h.window).map_err(|_| "X11 window ID exceeds u32".into())
                    }
                    RawWindowHandle::Xcb(h) => Ok(h.window.get()),
                    _ => Err(
                        "The GUI did not use the X11 backend. Wayland support is pending.".into(),
                    ),
                }
            })
            .map(Message::Native),
            Message::Opened(_) => Task::none(),
            Message::Native(Ok(xid)) => {
                self.xid = Some(xid);
                if self.pending_show {
                    self.show(None)
                } else {
                    Task::none()
                }
            }
            Message::Native(Err(error)) => {
                tracing::error!(%error);
                self.status = error;
                iced::exit()
            }
            Message::Target(Ok(target)) => {
                self.target = target;
                self.prepare()
            }
            Message::Target(Err(error)) => {
                self.status = error;
                self.prepare()
            }
            Message::Prepared(Ok(())) => {
                self.visible = true;
                self.mapped_at = Instant::now();
                window::set_mode(self.popup, window::Mode::Windowed)
                    .chain(Task::done(Message::Mapped))
            }
            Message::Prepared(Err(error)) => {
                self.status = error;
                Task::none()
            }
            Message::Mapped => {
                if let Some(xid) = self.xid {
                    self.services.platform.focus_popup(xid);
                }
                self.search_focused = true;
                widget::operation::focus(SEARCH_ID)
            }
            Message::Search(query) => {
                self.query = query;
                self.selected = None;
                self.search_focused = true;
                self.offset = 0.0;
                self.generation = self.generation.wrapping_add(1);
                let generation = self.generation;
                Task::perform(
                    async move {
                        tokio::time::sleep(Duration::from_millis(75)).await;
                        generation
                    },
                    Message::SearchDue,
                )
            }
            Message::SearchReady(generation, result) => {
                if generation != self.generation {
                    return Task::none();
                }
                match result {
                    Ok(ids) => {
                        self.ids = ids;
                        if !self.selected.is_some_and(|id| self.ids.contains(&id)) {
                            self.selected = self.ids.first().copied();
                        }
                        self.offset = self
                            .offset
                            .min((self.ids.len() as f32 * ROW - VIEWPORT).max(0.0));
                        self.load_page()
                    }
                    Err(error) => {
                        self.status = error;
                        Task::none()
                    }
                }
            }
            Message::PageReady(Ok(page)) => {
                if page.generation == self.generation
                    && self.page_pending == Some((page.generation, page.start))
                {
                    self.page_start = page.start;
                    self.page_pending = None;
                    self.summaries = page.summaries.into_iter().map(|s| (s.id, s)).collect();
                    self.thumbnails = page.thumbnails.into_iter().collect();
                }
                Task::none()
            }
            Message::PageReady(Err(error)) => {
                self.page_pending = None;
                self.status = error;
                Task::none()
            }
            Message::Scroll(offset) => {
                self.offset = offset;
                self.load_page()
            }
            Message::SearchFocused(focused) => {
                self.search_focused = focused;
                Task::none()
            }
            Message::Select(id) => {
                self.selected = Some(id);
                self.search_focused = false;
                let double = self.last_click.is_some_and(|(old, time)| {
                    old == id && time.elapsed() < Duration::from_millis(400)
                });
                self.last_click = Some((id, Instant::now()));
                if double {
                    self.last_click = None;
                    self.update(Message::Accept(false))
                } else {
                    widget::operation::focus("list-focus")
                }
            }
            Message::Accept(copy_only) => {
                if self.composing {
                    return Task::none();
                }
                let Some(id) = self.selected else {
                    return Task::none();
                };
                let store = self.services.store.clone();
                let platform = self.services.platform.clone();
                let target = self.target;
                let auto = self.settings.automatic_paste && !copy_only;
                let shortcut = self.settings.paste_shortcut;
                // Hide completes before background ownership/focus/paste work starts.
                self.hide(false).chain(Task::perform(
                    background(move || {
                        let offer = store
                            .call(move |repo| {
                                let offer = repo.offer(id)?;
                                repo.mark_used(id)?;
                                Ok(offer)
                            })
                            .map_err(|e| e.to_string())?;
                        if auto {
                            platform.paste(offer, target, shortcut)
                        } else {
                            platform.publish(offer)
                        }
                    }),
                    Message::Published,
                ))
            }
            Message::Published(result) => {
                self.status = match result {
                    Ok(()) => {
                        "Copied; press your paste shortcut if automatic paste is unavailable".into()
                    }
                    Err(error) => error,
                };
                Task::none()
            }
            Message::Settings => {
                if let Some(id) = self.settings_window {
                    window::gain_focus(id)
                } else {
                    let (id, task) = window::open(window::Settings {
                        size: Size::new(620.0, 720.0),
                        min_size: Some(Size::new(500.0, 500.0)),
                        exit_on_close_request: false,
                        platform_specific: window::settings::PlatformSpecific {
                            application_id: "io.github.gh2302.Feritto".into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    });
                    self.settings_window = Some(id);
                    task.map(Message::Opened)
                }
            }
            Message::Pause(value) => {
                self.settings.paused = value;
                self.status = if value {
                    "Capture paused"
                } else {
                    "Capture resumed"
                }
                .into();
                self.persist()
            }
            Message::AutoPaste(value) => {
                self.settings.automatic_paste = value;
                self.persist()
            }
            Message::Dark(value) => {
                self.settings.dark_theme = value;
                self.persist()
            }
            Message::Coexist(value) => {
                self.settings.coexistence = value;
                self.persist()
            }
            Message::Text(value) => {
                self.settings.capture.text = value;
                self.persist()
            }
            Message::Images(value) => {
                self.settings.capture.images = value;
                self.persist()
            }
            Message::Html(value) => {
                self.settings.capture.html = value;
                self.persist()
            }
            Message::Rtf(value) => {
                self.settings.capture.rtf = value;
                self.persist()
            }
            Message::Files(value) => {
                self.settings.capture.files = value;
                self.persist()
            }
            Message::Shortcut(value) => {
                self.settings.paste_shortcut = value;
                self.persist()
            }
            Message::MaxClips(value) => {
                if let Ok(value) = value.parse::<usize>() {
                    self.settings.max_clips = value.clamp(1, 100000);
                    return self.persist();
                }
                Task::none()
            }
            Message::Age(value) => {
                if let Ok(value) = value.parse::<u32>() {
                    self.settings.max_age_days = value.min(36500);
                    return self.persist();
                }
                Task::none()
            }
            Message::Star | Message::Delete => {
                let Some(id) = self.selected else {
                    return Task::none();
                };
                let star = matches!(message, Message::Star);
                let store = self.services.store.clone();
                Task::perform(
                    background(move || {
                        store
                            .call(move |r| {
                                if star {
                                    r.toggle_star(id)
                                } else {
                                    r.delete(id)
                                }
                            })
                            .map_err(|e| e.to_string())
                    }),
                    Message::Done,
                )
            }
            Message::ClearHistory => {
                self.confirm_clear = true;
                Task::none()
            }
            Message::ConfirmClearHistory => {
                self.confirm_clear = false;
                let store = self.services.store.clone();
                Task::perform(
                    background(move || {
                        store.call(|r| r.clear_history()).map_err(|e| e.to_string())
                    }),
                    Message::Done,
                )
            }
            Message::ClearClipboard => {
                let platform = self.services.platform.clone();
                Task::perform(background(move || platform.clear()), Message::Done)
            }
            Message::Done(Err(error)) => {
                self.status = error;
                Task::none()
            }
            Message::Done(Ok(())) => self.search(),
            Message::Quit => {
                let platform = self.services.platform.clone();
                Task::perform(
                    background(move || {
                        platform.shutdown();
                        Ok(())
                    }),
                    |_| Message::Exit,
                )
            }
            Message::Exit => iced::exit(),
            Message::Event(id, event, status) => self.event(id, event, status),
        }
    }
    fn event(
        &mut self,
        id: window::Id,
        event: Event,
        status: iced::event::Status,
    ) -> Task<Message> {
        if Some(id) == self.settings_window {
            if matches!(event, Event::Window(window::Event::CloseRequested)) {
                self.settings_window = None;
                return window::close(id);
            }
            if matches!(
                event,
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(Named::Escape),
                    ..
                })
            ) {
                self.settings_window = None;
                return window::close(id);
            }
            return Task::none();
        }
        if id != self.popup {
            return Task::none();
        }
        match event {
            Event::Window(window::Event::CloseRequested) => self.hide(true),
            Event::Window(window::Event::Unfocused)
                if self.visible
                    && self.settings_window.is_none()
                    && self.mapped_at.elapsed() > Duration::from_millis(200) =>
            {
                self.hide(false)
            }
            Event::InputMethod(iced::advanced::input_method::Event::Preedit(value, _)) => {
                self.composing = !value.is_empty();
                Task::none()
            }
            Event::InputMethod(
                iced::advanced::input_method::Event::Commit(_)
                | iced::advanced::input_method::Event::Closed,
            ) => {
                self.composing = false;
                Task::none()
            }
            Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                widget::operation::is_focused(SEARCH_ID).map(Message::SearchFocused)
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(Named::Tab),
                ..
            }) => widget::operation::is_focused(SEARCH_ID).map(Message::SearchFocused),
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                text: typed,
                ..
            }) if self.visible && !self.composing => match key {
                Key::Named(Named::Escape) => self.hide(true),
                Key::Named(Named::Enter) => self.update(Message::Accept(modifiers.shift())),
                Key::Named(Named::ArrowDown) => self.navigate(1),
                Key::Named(Named::ArrowUp) => self.navigate(-1),
                Key::Named(Named::PageDown) => self.navigate(5),
                Key::Named(Named::PageUp) => self.navigate(-5),
                Key::Named(Named::Home) if !self.search_focused => {
                    self.navigate(-(self.ids.len() as isize))
                }
                Key::Named(Named::End) if !self.search_focused => {
                    self.navigate(self.ids.len() as isize)
                }
                Key::Named(Named::Delete) if !self.search_focused => self.update(Message::Delete),
                Key::Character(c) if modifiers.control() && c.eq_ignore_ascii_case("l") => {
                    self.search_focused = true;
                    widget::operation::focus(SEARCH_ID)
                }
                Key::Character(c) if modifiers.control() && c == "," => {
                    self.update(Message::Settings)
                }
                Key::Character(_)
                    if !self.search_focused
                        && !modifiers.control()
                        && !modifiers.alt()
                        && status == iced::event::Status::Ignored =>
                {
                    if let Some(typed) = typed {
                        let mut query = self.query.clone();
                        query.push_str(&typed);
                        let search = self.update(Message::Search(query));
                        return Task::batch([search, widget::operation::focus(SEARCH_ID)]);
                    }
                    Task::none()
                }
                _ => Task::none(),
            },
            _ => Task::none(),
        }
    }
    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if Some(id) == self.settings_window {
            return self.settings_view();
        }
        let header = row![
            column![
                text("feritto").size(26),
                text(if self.settings.paused {
                    "Capture paused"
                } else {
                    "Your clipboard, within reach"
                })
                .size(12)
            ]
            .spacing(2),
            space().width(Fill),
            button(if self.settings.paused {
                "Resume"
            } else {
                "Pause"
            })
            .on_press(Message::Pause(!self.settings.paused))
            .style(button::secondary),
            button("Settings")
                .on_press(Message::Settings)
                .style(button::secondary)
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        let search = text_input("Search clipboard history…", &self.query)
            .id(SEARCH_ID)
            .on_input(Message::Search)
            .padding(12)
            .size(16);
        let start = ((self.offset / ROW).floor() as usize)
            .saturating_sub(OVERSCAN)
            .min(self.ids.len());
        let end = (start + (VIEWPORT / ROW).ceil() as usize + OVERSCAN * 2).min(self.ids.len());
        let mut rows = column![space().height(start as f32 * ROW)].spacing(0);
        for &id in &self.ids[start..end] {
            let summary = self.summaries.get(&id);
            let selected = self.selected == Some(id);
            let preview = summary
                .map(|s| bounded_text(&s.preview.replace(['\n', '\r'], " "), 180))
                .unwrap_or_else(|| "Loading…".into());
            let detail = summary
                .map(|s| {
                    format!(
                        "{}{} · {}",
                        if s.starred { "★ · " } else { "" },
                        if s.has_image { "Image" } else { "Clip" },
                        format_bytes(s.size)
                    )
                })
                .unwrap_or_default();
            let mut content = row![].spacing(8).align_y(iced::Alignment::Center);
            if let Some(handle) = self.thumbnails.get(&id) {
                content = content.push(
                    image(handle.clone())
                        .width(48)
                        .height(42)
                        .content_fit(iced::ContentFit::Contain),
                );
            }
            content = content.push(
                column![
                    text(preview)
                        .size(14)
                        .wrapping(widget::text::Wrapping::None),
                    text(detail).size(11)
                ]
                .spacing(3)
                .width(Fill),
            );
            let entry = button(content)
                .on_press(Message::Select(id))
                .width(Fill)
                .height(ROW)
                .padding(8)
                .style(move |theme: &Theme, status| {
                    if selected {
                        button::primary(theme, status)
                    } else {
                        button::text(theme, status)
                    }
                });
            rows = rows.push(entry);
        }
        rows = rows.push(space().height((self.ids.len() - end) as f32 * ROW));
        let list: Element<'_, Message> = if self.ids.is_empty() {
            container(
                column![
                    text(if self.query.is_empty() {
                        "Your history starts with a copy."
                    } else {
                        "No matching clips."
                    })
                    .size(18),
                    text("Text, screenshots, links, and rich clipboard formats.").size(13)
                ]
                .spacing(12),
            )
            .center(Fill)
            .height(VIEWPORT)
            .into()
        } else {
            scrollable(rows)
                .id(LIST_ID)
                .height(VIEWPORT)
                .on_scroll(|v| Message::Scroll(v.absolute_offset().y))
                .into()
        };
        let preview = self.preview_view();
        let body = row![
            container(list).width(Length::FillPortion(3)),
            container(preview).width(Length::FillPortion(2)).padding(10)
        ]
        .spacing(12)
        .height(VIEWPORT);
        let footer = row![
            text(format!("{} clips", self.ids.len())).size(12),
            space().width(Fill),
            button("Copy")
                .on_press(Message::Accept(true))
                .style(button::secondary),
            button("Paste ↵").on_press(Message::Accept(false))
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        container(
            column![
                header,
                search,
                body,
                footer,
                text(bounded_text(&self.status, 160)).size(11),
                text("↑↓ navigate · Enter paste · Shift+Enter copy · Esc close · Ctrl+L search")
                    .size(11)
            ]
            .spacing(12),
        )
        .padding(18)
        .width(Fill)
        .height(Fill)
        .into()
    }
    fn preview_view(&self) -> Element<'_, Message> {
        let Some(id) = self.selected else {
            return text("Select a clip to preview it.").size(13).into();
        };
        let Some(summary) = self.summaries.get(&id) else {
            return text("Loading preview…").size(13).into();
        };
        let mut content = column![
            row![
                text("Preview").size(16),
                space().width(Fill),
                button(if summary.starred { "★" } else { "☆" })
                    .on_press(Message::Star)
                    .style(button::text),
                button("Delete")
                    .on_press(Message::Delete)
                    .style(button::text)
            ]
            .spacing(4)
        ]
        .spacing(10);
        if let Some(handle) = self.thumbnails.get(&id) {
            content = content.push(
                image(handle.clone())
                    .width(Fill)
                    .height(150)
                    .content_fit(iced::ContentFit::Contain),
            );
        }
        content = content.push(
            scrollable(
                text(&summary.preview)
                    .size(13)
                    .shaping(widget::text::Shaping::Advanced),
            )
            .height(Fill),
        );
        if summary.search_truncated {
            content =
                content.push(text("Search covers the first 64 KiB of normalized text.").size(11));
        }
        content = content.push(text(summary.formats.join(" · ")).size(10));
        content.into()
    }
    fn settings_view(&self) -> Element<'_, Message> {
        let caps = self.services.platform.capabilities();
        let status = |name: &str, cap: Capability| {
            text(format!(
                "{name}: {}",
                match cap {
                    Capability::Available => "available".into(),
                    Capability::PermissionRequired(reason) =>
                        format!("permission required: {reason}"),
                    Capability::Unavailable(reason) => reason,
                }
            ))
            .size(12)
        };
        let content=column![
            text("Settings").size(28),text("Changes are saved automatically.").size(13),
            checkbox(self.settings.paused).label("Pause clipboard capture").on_toggle(Message::Pause),
            checkbox(self.settings.automatic_paste).label("Paste automatically after choosing a clip").on_toggle(Message::AutoPaste),
            row![button("Ctrl+V").on_press(Message::Shortcut(PasteShortcut::ControlV)),button("Ctrl+Shift+V").on_press(Message::Shortcut(PasteShortcut::ControlShiftV)),button("Shift+Insert (PRIMARY)").on_press(Message::Shortcut(PasteShortcut::ShiftInsert))].spacing(6),
            text(format!("Paste shortcut: {:?}",self.settings.paste_shortcut)).size(12),
            checkbox(self.settings.dark_theme).label("Dark theme").on_toggle(Message::Dark),
            checkbox(self.settings.coexistence).label("History only: let another manager retain the clipboard").on_toggle(Message::Coexist),
            text("Capture formats").size(18),
            row![checkbox(self.settings.capture.text).label("Text").on_toggle(Message::Text),checkbox(self.settings.capture.images).label("PNG images").on_toggle(Message::Images),checkbox(self.settings.capture.html).label("HTML").on_toggle(Message::Html)].spacing(16),
            row![checkbox(self.settings.capture.rtf).label("RTF").on_toggle(Message::Rtf),checkbox(self.settings.capture.files).label("File / URI lists").on_toggle(Message::Files)].spacing(16),
            text("Retention · starred clips are protected").size(18),
            row![column![text("Maximum clips").size(12),text_input("10000",&self.settings.max_clips.to_string()).on_input(Message::MaxClips).padding(8)],column![text("Maximum age in days (0 = unlimited)").size(12),text_input("30",&self.settings.max_age_days.to_string()).on_input(Message::Age).padding(8)]].spacing(12),
            text("Desktop capabilities").size(18),status("Backend",Capability::Unavailable(caps.backend)),status("Global shortcut (Ctrl+grave)",caps.global_hotkey),status("Automatic paste",caps.automatic_paste),status("Clipboard recovery",caps.persistence),status("Caret",caps.caret_placement),
            text("Wayland support is pending. Clipboard files are private to your user and are stored unencrypted.").size(12),
            row![button("Clear clipboard").on_press(Message::ClearClipboard).style(button::secondary),button(if self.confirm_clear {"Confirm: delete all history"}else{"Clear history…"}).on_press(if self.confirm_clear {Message::ConfirmClearHistory}else{Message::ClearHistory}).style(button::danger)].spacing(12),
            text(&self.status).size(12),button("Quit feritto").on_press(Message::Quit).style(button::secondary),
        ].spacing(14);
        container(scrollable(content))
            .padding(24)
            .width(Fill)
            .height(Fill)
            .into()
    }
}
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
