//! Run through scripts/check-wayland.sh, which starts a private headless compositor:
//! cargo test -p feritto-platform --test wayland_protocol -- --ignored --test-threads=1
//! The tests share one compositor clipboard, so they must not run in parallel.
use anyhow::{Context, Result};
use crossbeam_channel::Receiver;
use feritto_core::*;
use feritto_platform::*;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat::WlSeat},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

fn start(settings: Settings) -> Result<(WaylandService, Receiver<PlatformEvent>)> {
    // SAFETY: set before any service threads exist in this test process.
    unsafe { std::env::set_var("FERITTO_NO_PORTALS", "1") };
    WaylandService::start(settings, PlatformOptions::default())
}

fn next_capture(events: &Receiver<PlatformEvent>, within: Duration) -> Option<Capture> {
    let end = Instant::now() + within;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        if let Ok(PlatformEvent::Captured(c)) = events.recv_timeout(left) {
            return Some(c);
        }
    }
    None
}

fn wait_capture(events: &Receiver<PlatformEvent>) -> Capture {
    next_capture(events, Duration::from_secs(15)).expect("no clipboard capture arrived")
}

fn next_paste_result(events: &Receiver<PlatformEvent>) -> String {
    let end = Instant::now() + Duration::from_secs(5);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        if let Ok(PlatformEvent::PasteResult(status)) = events.recv_timeout(left) {
            return status;
        }
    }
    panic!("no paste result arrived");
}

fn paste(args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("wl-paste")
        .args(["--no-newline"])
        .args(args)
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

fn wait_clipboard(expected: &[u8], args: &[&str]) {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if paste(args).as_deref() == Some(expected) {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "clipboard did not match: {:?}",
        paste(args).map(|b| String::from_utf8_lossy(&b[..b.len().min(80)]).into_owned())
    );
}

fn copy_foreground(data: &[u8], mime: Option<&str>) -> Result<Child> {
    let mut command = Command::new("wl-copy");
    command.arg("--foreground");
    if let Some(mime) = mime {
        command.args(["--type", mime]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child.stdin.take().unwrap().write_all(data)?;
    Ok(child)
}

/// A data-control client that owns the clipboard with arbitrary types until dropped. Unlike
/// wl-copy it can combine formats, including password-manager and ignore hints.
struct TestSource {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

struct SourceState {
    data: Vec<(String, Vec<u8>)>,
}
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for SourceState {
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
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for SourceState {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlSeat, ZwlrDataControlManagerV1, ZwlrDataControlOfferV1);
impl Dispatch<ZwlrDataControlDeviceV1, ()> for SourceState {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        _: <ZwlrDataControlDeviceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
    wayland_client::event_created_child!(SourceState, ZwlrDataControlDeviceV1, [
        wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}
impl Dispatch<ZwlrDataControlSourceV1, ()> for SourceState {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_source_v1::Event::Send { mime_type, fd } = event
            && let Some((_, bytes)) = state.data.iter().find(|(m, _)| *m == mime_type)
        {
            let bytes = bytes.clone();
            // Write off-thread so a slow reader never stalls this client's event loop.
            thread::spawn(move || {
                let mut file = std::fs::File::from(fd);
                let _ = file.write_all(&bytes);
            });
        }
    }
}

impl TestSource {
    fn own(data: &[(&str, &[u8])]) -> Result<Self> {
        let data: Vec<_> = data
            .iter()
            .map(|(m, b)| (m.to_string(), b.to_vec()))
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            let run = || -> Result<()> {
                let conn = Connection::connect_to_env()?;
                let (globals, mut queue) = registry_queue_init::<SourceState>(&conn)?;
                let qh = queue.handle();
                let seat: WlSeat = globals.bind(&qh, 1..=1, ())?;
                let manager: ZwlrDataControlManagerV1 = globals.bind(&qh, 1..=2, ())?;
                let device = manager.get_data_device(&seat, &qh, ());
                let source = manager.create_data_source(&qh, ());
                for (mime, _) in &data {
                    source.offer(mime.clone());
                }
                device.set_selection(Some(&source));
                let mut state = SourceState { data };
                queue.roundtrip(&mut state)?;
                ready_tx.send(()).ok();
                while !flag.load(Ordering::Relaxed) {
                    queue.flush()?;
                    if let Some(guard) = queue.prepare_read() {
                        let fd = guard.connection_fd();
                        let mut fds = [rustix::event::PollFd::new(
                            &fd,
                            rustix::event::PollFlags::IN,
                        )];
                        let timeout = rustix::event::Timespec {
                            tv_sec: 0,
                            tv_nsec: 20_000_000,
                        };
                        let readable = rustix::event::poll(&mut fds, Some(&timeout))? > 0;
                        if readable {
                            guard.read()?;
                        }
                    }
                    queue.dispatch_pending(&mut state)?;
                }
                Ok(())
            };
            if let Err(error) = run() {
                eprintln!("test source failed: {error:#}");
            }
            // Dropping the connection destroys the source, like an application exiting.
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .context("test source did not take the clipboard")?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for TestSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn text_of(capture: &Capture) -> String {
    capture.offer.text_content().unwrap_or_default()
}

#[test]
#[ignore = "requires a private data-control compositor and wl-clipboard"]
fn capture_serve_recover_clear_and_pause() -> Result<()> {
    let (service, events) = start(Settings::default())?;
    let caps = service.capabilities();
    assert_eq!(caps.monitor, Capability::Available, "{caps:?}");
    assert_eq!(caps.ownership, Capability::Available);
    assert!(caps.backend.contains("data-control"), "{}", caps.backend);
    // The initial selection (if any) is delivered at startup; drain it.
    while next_capture(&events, Duration::from_millis(300)).is_some() {}

    let mut alpha = copy_foreground(b"alpha \xc3\xa9", Some("text/plain;charset=utf-8"))?;
    let capture = wait_capture(&events);
    assert_eq!(text_of(&capture), "alpha é");
    assert!(capture.complete);
    assert_eq!(
        capture.offer.representations[0].mime.as_deref(),
        Some("text/plain;charset=utf-8")
    );

    // Our own offer is served under every text alias and is never captured again.
    service
        .publish(Arc::new(Offer::text("beta ü")))
        .map_err(anyhow::Error::msg)?;
    wait_clipboard("beta ü".as_bytes(), &[]);
    for mime in ["text/plain", "UTF8_STRING", "TEXT"] {
        assert_eq!(paste(&["--type", mime]).unwrap(), "beta ü".as_bytes());
    }
    assert_eq!(paste(&["--type", "STRING"]).unwrap(), b"beta \xfc");
    assert!(next_capture(&events, Duration::from_millis(400)).is_none());
    alpha.kill().ok();
    alpha.wait().ok();

    // A fully captured clipboard survives its source exiting.
    let mut gamma = copy_foreground(b"gamma survives", None)?;
    assert_eq!(text_of(&wait_capture(&events)), "gamma survives");
    gamma.kill()?;
    gamma.wait()?;
    wait_clipboard(b"gamma survives", &[]);

    // An explicit clear by feritto stays cleared.
    service.clear().map_err(anyhow::Error::msg)?;
    thread::sleep(Duration::from_millis(400));
    assert_eq!(paste(&[]), None);

    // A 20 MiB transfer is captured completely and restored byte-for-byte.
    let large: Vec<u8> = (0..20 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let mut source = copy_foreground(&large, Some("application/x-test-large"))?;
    let settings = Settings {
        capture: CapturePolicy {
            extra_targets: vec!["application/x-test-large".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    service.configure(settings.clone());
    // wl-copy started before the extra target was configured; copy again to capture it.
    source.kill()?;
    source.wait()?;
    thread::sleep(Duration::from_millis(200));
    while next_capture(&events, Duration::from_millis(200)).is_some() {}
    let mut source = copy_foreground(&large, Some("application/x-test-large"))?;
    let capture = wait_capture(&events);
    assert!(capture.complete, "{:?}", capture.omitted);
    assert_eq!(capture.offer.representations.len(), 1);
    assert!(capture.offer.representations[0].bytes.as_ref() == large.as_slice());
    source.kill()?;
    source.wait()?;
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        if paste(&["--type", "application/x-test-large"]).as_deref() == Some(large.as_slice()) {
            break;
        }
        assert!(Instant::now() < end, "large clip was not restored");
        thread::sleep(Duration::from_millis(100));
    }

    // Pause stops capture; a paused generation cannot resurrect older data on exit.
    service.set_paused(true).map_err(anyhow::Error::msg)?;
    service.configure(Settings {
        paused: true,
        ..settings.clone()
    });
    let mut paused = copy_foreground(b"while paused", None)?;
    assert!(next_capture(&events, Duration::from_millis(500)).is_none());
    paused.kill()?;
    paused.wait()?;
    thread::sleep(Duration::from_millis(300));
    assert_eq!(paste(&[]), None);
    service.configure(settings);
    let mut resumed = copy_foreground(b"resumed", None)?;
    assert_eq!(text_of(&wait_capture(&events)), "resumed");
    resumed.kill().ok();
    resumed.wait().ok();
    service.shutdown();
    Ok(())
}

#[test]
#[ignore = "requires a private data-control compositor and wl-clipboard"]
fn multiple_formats_hints_and_primary() -> Result<()> {
    let settings = Settings {
        capture: CapturePolicy {
            extra_targets: vec!["application/x-test-opaque".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (service, events) = start(settings)?;
    while next_capture(&events, Duration::from_millis(300)).is_some() {}

    let opaque: &[u8] = &[0, 159, 146, 150, 255, 1];
    let rich = TestSource::own(&[
        ("text/plain;charset=utf-8", "multi".as_bytes()),
        ("text/html", b"<i>multi</i>"),
        ("text/rtf", b"{\\rtf1 multi}"),
        ("text/uri-list", b"file:///tmp/multi\r\n"),
        ("application/x-test-opaque", opaque),
        ("application/x-unconfigured-private", b"never requested"),
    ])?;
    let capture = wait_capture(&events);
    let formats: Vec<_> = capture
        .offer
        .representations
        .iter()
        .map(|r| r.target.as_str())
        .collect();
    assert_eq!(
        formats,
        [
            "text/plain;charset=utf-8",
            "text/html",
            "text/rtf",
            "text/uri-list",
            "application/x-test-opaque"
        ]
    );
    assert!(capture.complete);
    let opaque_capture = capture
        .offer
        .representations
        .iter()
        .find(|r| r.target == "application/x-test-opaque")
        .unwrap();
    assert_eq!(opaque_capture.bytes.as_ref(), opaque);
    // After the source exits, every captured format is served unchanged.
    drop(rich);
    wait_clipboard(b"<i>multi</i>", &["--type", "text/html"]);
    assert_eq!(
        paste(&["--type", "application/x-test-opaque"]).unwrap(),
        opaque
    );
    assert_eq!(
        paste(&["--type", "application/x-unconfigured-private"]),
        None
    );

    // A secret-hinted generation is never captured and cannot resurrect the older clip.
    let secret = TestSource::own(&[
        ("text/plain;charset=utf-8", b"hunter2"),
        ("x-kde-passwordManagerHint", b"secret"),
    ])?;
    assert!(next_capture(&events, Duration::from_millis(600)).is_none());
    drop(secret);
    thread::sleep(Duration::from_millis(400));
    assert_eq!(paste(&[]), None);

    // A non-secret hint value does not block capture.
    let hinted = TestSource::own(&[
        ("text/plain;charset=utf-8", b"not secret"),
        ("x-kde-passwordManagerHint", b"other"),
    ])?;
    assert_eq!(text_of(&wait_capture(&events)), "not secret");
    drop(hinted);

    let ignored = TestSource::own(&[
        ("text/plain;charset=utf-8", b"ignored"),
        ("application/x-feritto-ignore", b""),
    ])?;
    assert!(next_capture(&events, Duration::from_millis(600)).is_none());
    drop(ignored);

    // Shift+Insert publishes PRIMARY as well as CLIPBOARD; PRIMARY stays independent.
    service
        .paste(
            Arc::new(Offer::text("primary too")),
            PasteTarget::default(),
            PasteShortcut::ShiftInsert,
        )
        .map_err(anyhow::Error::msg)?;
    wait_clipboard(b"primary too", &["--primary"]);
    wait_clipboard(b"primary too", &[]);
    // Without a confirmed target (or any injector), no keys are sent and the user is told
    // to paste manually.
    let status = next_paste_result(&events);
    assert!(status.starts_with("Copied; press Shift+Insert"), "{status}");
    service
        .publish(Arc::new(Offer::text("clipboard only")))
        .map_err(anyhow::Error::msg)?;
    wait_clipboard(b"clipboard only", &[]);
    assert_eq!(paste(&["--primary"]).unwrap(), b"primary too");
    service.shutdown();
    Ok(())
}

#[test]
#[ignore = "requires a private wlroots compositor, wev, and stdbuf"]
fn verified_paste_reaches_the_target_window() -> Result<()> {
    let (service, events) = start(Settings::default())?;
    let caps = service.capabilities();
    assert_eq!(caps.automatic_paste, Capability::Available, "{caps:?}");
    let mut wev = Command::new("stdbuf")
        .args([
            "-oL",
            "wev",
            "-f",
            "wl_keyboard:key",
            "-f",
            "wl_keyboard:modifiers",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = wev.stdout.take().unwrap();
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    // The compositor focuses the new window; foreign-toplevel reports it as the target.
    let end = Instant::now() + Duration::from_secs(5);
    let target = loop {
        let target = service.active_window().map_err(anyhow::Error::msg)?;
        if target.window != 0 {
            break target;
        }
        assert!(Instant::now() < end, "no active target window");
        thread::sleep(Duration::from_millis(50));
    };
    service
        .paste(
            Arc::new(Offer::text("pasted")),
            target,
            PasteShortcut::ControlV,
        )
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        next_paste_result(&events),
        "Pasted into the previous window"
    );
    let mut control = false;
    let mut key = false;
    let end = Instant::now() + Duration::from_secs(3);
    while !(control && key) && Instant::now() < end {
        if let Ok(line) = line_rx.recv_timeout(Duration::from_millis(100)) {
            control |= line.contains("Control");
            key |= line.contains("sym: v");
        }
    }
    wev.kill().ok();
    wev.wait().ok();
    assert!(control && key, "wev did not receive Ctrl+V");
    wait_clipboard(b"pasted", &[]);

    // A target that no longer exists is never typed into.
    service
        .paste(
            Arc::new(Offer::text("closed")),
            target,
            PasteShortcut::ControlV,
        )
        .map_err(anyhow::Error::msg)?;
    let status = next_paste_result(&events);
    assert!(status.contains("cannot be confirmed"), "{status}");
    service.shutdown();
    Ok(())
}
