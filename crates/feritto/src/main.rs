mod ipc;
mod tray;
use anyhow::{Context, Result, bail};
use feritto_core::*;
use feritto_platform::{Backend, Platform, PlatformEvent, PlatformOptions};
use feritto_store::{Store, private_dir};
use feritto_ui::{Control, Services};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", content = "argument", rename_all = "kebab-case")]
pub enum Request {
    Show,
    Toggle,
    Hide,
    Settings,
    Quit,
    Pause,
    Status,
    List(String),
    Copy(ClipId),
    Paste(ClipId),
    Add(String),
    Delete(ClipId),
    ClearHistory,
    ClearClipboard,
}
/// The IPC wire message. A launcher or compositor binding may give the CLI process an
/// xdg-activation token; forwarding it lets the service's popup take focus on Wayland.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(flatten)]
    pub request: Request,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_token: Option<String>,
}
#[derive(Clone)]
pub struct ContextData {
    store: Store,
    platform: Platform,
    settings: Arc<Mutex<Settings>>,
    controls: crossbeam_channel::Sender<Control>,
}
impl ContextData {
    fn handle_envelope(&self, envelope: Envelope) -> Result<Value> {
        self.handle(envelope.request, envelope.activation_token)
    }
    /// `token` activates the window that a Show, Toggle, or Settings request opens.
    fn handle(&self, request: Request, token: Option<String>) -> Result<Value> {
        match request {
            Request::Status => Ok(
                json!({"running":true,"version":env!("CARGO_PKG_VERSION"),"clips":self.store.call(|r|r.count())?,"paused":self.settings.lock().unwrap().paused,"capabilities":self.platform.capabilities()}),
            ),
            Request::List(query) => self.store.call(move |r| {
                let ids = r.search_ids(&query)?;
                Ok(serde_json::to_value(
                    r.summaries(&ids[..ids.len().min(1000)])?,
                )?)
            }),
            Request::Copy(id) | Request::Paste(id) => {
                let paste = matches!(request, Request::Paste(_));
                let offer = self.store.call(move |r| {
                    let offer = r.offer(id)?;
                    r.mark_used(id)?;
                    Ok(offer)
                })?;
                if paste {
                    let target = self.platform.active_window().map_err(anyhow::Error::msg)?;
                    let shortcut = self.settings.lock().unwrap().paste_shortcut;
                    self.platform
                        .paste(offer, target, shortcut)
                        .map_err(anyhow::Error::msg)?;
                } else {
                    self.platform.publish(offer).map_err(anyhow::Error::msg)?;
                }
                Ok(json!({"copied":id,"automatic_paste_requested":paste}))
            }
            Request::Add(text) => {
                if text.len() > 32 * 1024 * 1024 {
                    bail!("text exceeds the 32 MiB limit")
                }
                let settings = self.settings.lock().unwrap().clone();
                let capture = Capture {
                    generation: 0,
                    offer: Arc::new(Offer::text(text)),
                    source: SourceIdentity::default(),
                    complete: true,
                    omitted: vec![],
                };
                let id = self.store.call(move |r| r.insert(&capture, &settings))?;
                self.controls.send(Control::HistoryChanged)?;
                Ok(json!({"id":id}))
            }
            Request::Delete(id) => {
                self.store.call(move |r| r.delete(id))?;
                self.controls.send(Control::HistoryChanged)?;
                Ok(json!({"deleted":id}))
            }
            Request::ClearHistory => {
                self.store.call(|r| r.clear_history())?;
                self.controls.send(Control::HistoryChanged)?;
                Ok(json!({"cleared":"history"}))
            }
            Request::ClearClipboard => {
                self.platform.clear().map_err(anyhow::Error::msg)?;
                Ok(json!({"cleared":"clipboard"}))
            }
            request => {
                let control = match request {
                    Request::Show => Control::Show(token),
                    Request::Toggle => Control::Toggle(
                        self.platform.active_window().map_err(anyhow::Error::msg)?,
                        token,
                    ),
                    Request::Hide => Control::Hide,
                    Request::Settings => Control::Settings(token),
                    Request::Quit => Control::Quit,
                    Request::Pause => Control::Pause,
                    _ => unreachable!(),
                };
                self.controls.send(control)?;
                Ok(json!({"ok":true}))
            }
        }
    }
}
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "feritto=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    if let Err(error) = run() {
        eprintln!("feritto: {error:#}");
        std::process::exit(1)
    }
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--help" || s == "-h") {
        print_help();
        return Ok(());
    }
    if args.first().is_some_and(|s| s == "--version" || s == "-V") {
        println!("feritto {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let (request, daemon) = parse_args(&args)?;
    let paths = Paths::new()?;
    let envelope = Envelope {
        activation_token: matches!(request, Request::Show | Request::Toggle | Request::Settings)
            .then(|| std::env::var("XDG_ACTIVATION_TOKEN").ok())
            .flatten()
            .filter(|token| !token.is_empty()),
        request: request.clone(),
    };
    if let Some(response) = ipc::forward(&paths.socket, &envelope)? {
        print_response(response)?;
        return Ok(());
    }
    if !matches!(request, Request::Show | Request::Toggle | Request::Settings) && !daemon {
        bail!("feritto is not running; start it with feritto --daemon")
    }
    let backend = feritto_platform::detect_session()?;
    let instance = ipc::Instance::claim(&paths.runtime, &paths.socket)?;
    if instance.is_none() {
        if let Some(response) = ipc::forward(&paths.socket, &envelope)? {
            print_response(response)?;
            return Ok(());
        }
        bail!("another feritto instance is starting; retry the command")
    }
    let _instance = instance.unwrap();
    private_dir(&paths.config)?;
    let settings_path = paths.config.join("settings.json");
    let mut settings = if settings_path.exists() {
        serde_json::from_slice::<Settings>(&std::fs::read(&settings_path)?)
            .context("invalid settings.json")?
    } else {
        Settings::default()
    };
    settings.max_clips = settings.max_clips.clamp(1, 100000);
    settings.capture.max_target_bytes = settings
        .capture
        .max_target_bytes
        .clamp(1024, 128 * 1024 * 1024);
    settings.capture.max_generation_bytes = settings
        .capture
        .max_generation_bytes
        .clamp(settings.capture.max_target_bytes, 256 * 1024 * 1024);
    settings.capture.progress_timeout_ms = settings.capture.progress_timeout_ms.clamp(100, 60000);
    settings.capture.generation_timeout_ms = settings
        .capture
        .generation_timeout_ms
        .clamp(settings.capture.progress_timeout_ms, 120000);
    feritto_store::write_private(&settings_path, &serde_json::to_vec_pretty(&settings)?)?;
    let store = Store::open(paths.data)?;
    let cleanup = settings.clone();
    store.call(move |r| r.cleanup(&cleanup))?;
    let (platform, events) = Platform::start(
        settings.clone(),
        PlatformOptions {
            state_dir: paths.config.clone(),
        },
    )?;
    let settings = Arc::new(Mutex::new(settings));
    let (controls, control_rx) = crossbeam_channel::bounded(128);
    let context = ContextData {
        store: store.clone(),
        platform: platform.clone(),
        settings: settings.clone(),
        controls: controls.clone(),
    };
    ipc::serve(_instance.listener.try_clone()?, context.clone())?;
    let bridge = context.clone();
    thread::Builder::new()
        .name("feritto-events".into())
        .spawn(move || {
            while let Ok(event) = events.recv() {
                let control = match event {
                    PlatformEvent::Captured(capture) => {
                        let settings = bridge.settings.lock().unwrap().clone();
                        match bridge.store.call(move |r| r.insert(&capture, &settings)) {
                            Ok(_) => Control::HistoryChanged,
                            Err(error) => {
                                tracing::error!(%error,"history write failed");
                                Control::Status(format!("Could not save clipboard: {error}"))
                            }
                        }
                    }
                    PlatformEvent::Toggle(target, token) => Control::Toggle(target, token),
                    PlatformEvent::Status(status) | PlatformEvent::PasteResult(status) => {
                        Control::Status(status)
                    }
                };
                if bridge.controls.send(control).is_err() {
                    break;
                }
            }
        })?;
    tray::start(controls.clone());
    // A launcher's token belongs to the first window this process opens for the request.
    let mut startup_token = envelope.activation_token;
    if matches!(request, Request::Settings) {
        controls.send(Control::Settings(startup_token.take()))?;
    }
    tracing::info!(
        backend = match backend {
            Backend::X11 => "X11",
            Backend::Wayland => "Wayland",
        },
        renderer = if cfg!(feature = "wgpu") {
            "wgpu with tiny-skia fallback"
        } else {
            "tiny-skia"
        },
        "desktop service started"
    );
    feritto_ui::run(
        Services {
            store,
            platform: platform.clone(),
            controls: control_rx,
            settings,
            settings_path,
        },
        !daemon && !matches!(request, Request::Settings),
        startup_token.filter(|_| !daemon),
    )?;
    platform.shutdown();
    Ok(())
}
fn print_response(response: Value) -> Result<()> {
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        bail!("{error}")
    }
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}
fn parse_args(args: &[String]) -> Result<(Request, bool)> {
    let Some(first) = args.first().map(String::as_str) else {
        return Ok((Request::Show, false));
    };
    let one = |request| -> Result<_> {
        if args.len() != 1 {
            bail!("unexpected argument; use --help")
        }
        Ok((request, false))
    };
    let value = || args.get(1).context("missing argument; use --help");
    match first {
        "--daemon" => {
            if args.len() != 1 {
                bail!("unexpected argument")
            }
            Ok((Request::Show, true))
        }
        "--show" => one(Request::Show),
        "--toggle" => one(Request::Toggle),
        "--hide" => one(Request::Hide),
        "--settings" => one(Request::Settings),
        "--quit" => one(Request::Quit),
        "--pause" => one(Request::Pause),
        "--status" => one(Request::Status),
        "--clear-history" => one(Request::ClearHistory),
        "--clear-clipboard" => one(Request::ClearClipboard),
        "--list" => {
            if args.len() == 1 {
                one(Request::List(String::new()))
            } else if args.len() == 3 && args[1] == "--search" {
                Ok((Request::List(args[2].clone()), false))
            } else {
                bail!("use --list [--search QUERY]")
            }
        }
        "--add" | "--copy" | "--paste" | "--delete" => {
            if args.len() != 2 {
                bail!("expected exactly one argument")
            }
            let arg = value()?;
            Ok((
                match first {
                    "--add" => Request::Add(arg.clone()),
                    "--copy" => Request::Copy(arg.parse().context("invalid clip ID")?),
                    "--paste" => Request::Paste(arg.parse().context("invalid clip ID")?),
                    _ => Request::Delete(arg.parse().context("invalid clip ID")?),
                },
                false,
            ))
        }
        "--add-stdin" => {
            if args.len() != 1 {
                bail!("unexpected argument")
            }
            let mut text = String::new();
            std::io::stdin()
                .take(32 * 1024 * 1024 + 1)
                .read_to_string(&mut text)?;
            if text.len() > 32 * 1024 * 1024 {
                bail!("stdin exceeds 32 MiB")
            }
            Ok((Request::Add(text), false))
        }
        _ => bail!("unknown option {first}; use --help"),
    }
}
struct Paths {
    data: PathBuf,
    config: PathBuf,
    runtime: PathBuf,
    socket: PathBuf,
}
impl Paths {
    fn new() -> Result<Self> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let xdg = |key: &str, fallback: PathBuf| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or(fallback)
        };
        let data = xdg("XDG_DATA_HOME", home.join(".local/share")).join("feritto");
        let config = xdg("XDG_CONFIG_HOME", home.join(".config")).join("feritto");
        let runtime = xdg(
            "XDG_RUNTIME_DIR",
            std::env::temp_dir().join(format!("feritto-{}", rustix::process::getuid().as_raw())),
        )
        .join("feritto");
        private_dir(&runtime)?;
        let socket = runtime.join("service.sock");
        Ok(Self {
            data,
            config,
            runtime,
            socket,
        })
    }
}
fn print_help() {
    println!(
        "feritto — clipboard history for X11 and Wayland\n\nUsage: feritto [OPTION]\n\n  (no option)          Start or show the popup\n  --daemon             Start with the popup hidden\n  --toggle / --show     Control the popup (default shortcut: Ctrl+grave)\n                       On Wayland, bind `feritto --toggle` in the compositor if\n                       the desktop has no GlobalShortcuts portal\n  --hide / --settings   Hide the popup or open settings\n  --pause              Toggle capture pause\n  --status             Print capabilities and history count as JSON\n  --list [--search Q]   List up to 1000 clip summaries as JSON\n  --copy ID            Put a historical clip on the clipboard\n  --paste ID           Copy and request paste into the active window\n  --add TEXT           Add a text clip to history\n  --add-stdin           Add UTF-8 text from stdin (32 MiB maximum)\n  --delete ID          Delete one history entry\n  --clear-history      Delete all history; keep the current clipboard\n  --clear-clipboard    Clear CLIPBOARD; keep history\n  --quit               Stop the service\n  --help / --version"
    );
}
