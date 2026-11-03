//! Run with xvfb-run -a cargo test -p feritto-platform --test x11_protocol -- --ignored.
use anyhow::Result;
use feritto_core::*;
use feritto_platform::*;
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use x11rb::{connection::Connection, protocol::xproto::*};

fn wait_capture(events: &crossbeam_channel::Receiver<PlatformEvent>) -> Capture {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if let Ok(PlatformEvent::Captured(c)) = events.recv_timeout(Duration::from_millis(100)) {
            return c;
        }
    }
    panic!("no clipboard capture arrived");
}
fn wait_text(expected: &[u8]) {
    let end = Instant::now() + Duration::from_secs(4);
    while Instant::now() < end {
        if let Ok(out) = Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output()
            && out.status.success()
            && out.stdout == expected
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("clipboard did not match expected data");
}
#[test]
#[ignore = "requires an isolated native X11 server and xclip"]
fn capture_serve_large_persist_clear_and_secret() -> Result<()> {
    let (service, events) = X11Service::start(Settings::default())?;
    let (conn, _) = x11rb::connect(None)?;
    let clipboard = conn.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
    // xclip uses INCR for this 20 MiB fixture. Keep the actual source alive until capture finishes.
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard", "-i", "-quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let bytes = vec![b'x'; 20 * 1024 * 1024];
    child.stdin.take().unwrap().write_all(&bytes)?;
    let captured = wait_capture(&events);
    assert!(captured.complete);
    assert_eq!(captured.offer.text_content().unwrap().as_bytes(), bytes);
    let owner = conn.get_selection_owner(clipboard)?.reply()?.owner;
    child.kill()?;
    child.wait()?;
    wait_text(&bytes); // outgoing INCR after source destruction
    assert_ne!(conn.get_selection_owner(clipboard)?.reply()?.owner, owner);
    // Explicit clear must not restore the saved generation.
    conn.set_selection_owner(0u32, clipboard, 0u32)?.check()?;
    conn.flush()?;
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(conn.get_selection_owner(clipboard)?.reply()?.owner, 0);
    service
        .publish(Arc::new(Offer::text("history selection")))
        .unwrap();
    wait_text(b"history selection");
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !events
            .try_iter()
            .any(|e| matches!(e, PlatformEvent::Captured(_))),
        "own offers must not recapture"
    );
    // MULTIPLE: one supported conversion, one unsupported conversion, timestamp, and TARGETS.
    let window = conn.generate_id()?;
    let root = conn.setup().roots[0].root;
    conn.create_window(
        0,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )?
    .check()?;
    let atom = |name: &[u8]| conn.intern_atom(false, name).unwrap().reply().unwrap().atom;
    let multiple = atom(b"MULTIPLE");
    let pairs = atom(b"ATOM_PAIR");
    let property = atom(b"TEST_MULTIPLE");
    let text = atom(b"UTF8_STRING");
    let text_prop = atom(b"TEST_TEXT");
    let unsupported = atom(b"application/unsupported");
    let bad_prop = atom(b"TEST_BAD");
    use x11rb::wrapper::ConnectionExt as _;
    conn.change_property32(
        PropMode::REPLACE,
        window,
        property,
        pairs,
        &[text, text_prop, unsupported, bad_prop],
    )?;
    conn.convert_selection(window, clipboard, multiple, property, 0u32)?;
    conn.flush()?;
    loop {
        if let x11rb::protocol::Event::SelectionNotify(e) = conn.wait_for_event()? {
            assert_eq!(e.property, property);
            break;
        }
    }
    let result = conn
        .get_property(false, window, property, pairs, 0, 4)?
        .reply()?;
    assert_eq!(
        result.value32().unwrap().collect::<Vec<_>>(),
        vec![text, text_prop, unsupported, 0]
    );
    assert_eq!(
        conn.get_property(false, window, text_prop, text, 0, 1024)?
            .reply()?
            .value,
        b"history selection"
    );
    // A secret generation must not be fetched, persisted, or restore the previous clip.
    let secret = atom(b"x-kde-passwordManagerHint");
    let targets = atom(b"TARGETS");
    conn.set_selection_owner(window, clipboard, 0u32)?.check()?;
    conn.flush()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut checked = false;
    while Instant::now() < deadline && !checked {
        if let Some(x11rb::protocol::Event::SelectionRequest(e)) = conn.poll_for_event()? {
            if e.target == targets {
                conn.change_property32(
                    PropMode::REPLACE,
                    e.requestor,
                    e.property,
                    AtomEnum::ATOM,
                    &[targets, text, secret],
                )?;
            } else if e.target == secret {
                conn.change_property8(
                    PropMode::REPLACE,
                    e.requestor,
                    e.property,
                    secret,
                    b"secret",
                )?;
                checked = true;
            } else {
                panic!("secret clipboard content was requested")
            }
            conn.send_event(
                false,
                e.requestor,
                EventMask::NO_EVENT,
                SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: e.time,
                    requestor: e.requestor,
                    selection: e.selection,
                    target: e.target,
                    property: e.property,
                },
            )?;
            conn.flush()?;
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(checked);
    std::thread::sleep(Duration::from_millis(50));
    conn.destroy_window(window)?.check()?;
    conn.flush()?;
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(conn.get_selection_owner(clipboard)?.reply()?.owner, 0);
    assert!(
        !events
            .try_iter()
            .any(|e| matches!(e, PlatformEvent::Captured(_)))
    );
    service.clear().unwrap();
    assert_eq!(conn.get_selection_owner(clipboard)?.reply()?.owner, 0);
    service.shutdown();
    Ok(())
}
