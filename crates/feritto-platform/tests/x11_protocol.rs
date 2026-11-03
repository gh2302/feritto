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
    // Explicit paste publishes PRIMARY independently. A later CLIPBOARD copy must leave it intact.
    service
        .paste(
            Arc::new(Offer::text("primary selection")),
            PasteTarget::default(),
            PasteShortcut::ShiftInsert,
        )
        .unwrap();
    service
        .publish(Arc::new(Offer::text("new clipboard selection")))
        .unwrap();
    assert_eq!(
        Command::new("xclip")
            .args(["-selection", "primary", "-o"])
            .output()?
            .stdout,
        b"primary selection"
    );
    wait_text(b"new clipboard selection");
    service.clear().unwrap();
    assert_eq!(
        Command::new("xclip")
            .args(["-selection", "primary", "-o"])
            .output()?
            .stdout,
        b"primary selection"
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
    // One real owner exposes several formats plus private and alias targets. Only configured data is fetched.
    let source = conn.generate_id()?;
    conn.create_window(
        0,
        source,
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
    let html = atom(b"text/html");
    let rtf = atom(b"text/rtf");
    let uri = atom(b"text/uri-list");
    let opaque = atom(b"application/x-feritto-fixture");
    let private = atom(b"application/x-private");
    let formats = vec![
        (text, "Unicode café 世界".as_bytes().to_vec()),
        (html, b"<b>cafe</b>".to_vec()),
        (rtf, b"{\\rtf1 cafe}".to_vec()),
        (uri, b"file:///tmp/fixture\r\n".to_vec()),
        (opaque, vec![0, 255, 1, 0, 42]),
    ];
    let mut settings = Settings::default();
    settings.capture.extra_targets = vec!["application/x-feritto-fixture".into()];
    service.configure(settings);
    service.active_window().unwrap(); // command barrier
    conn.set_selection_owner(source, clipboard, 0u32)?.check()?;
    conn.flush()?;
    let end = Instant::now() + Duration::from_secs(5);
    let mut captured = None;
    while Instant::now() < end && captured.is_none() {
        while let Some(event) = conn.poll_for_event()? {
            if let x11rb::protocol::Event::SelectionRequest(e) = event {
                respond(&conn, e, targets, &formats, Some(private))?;
            }
        }
        while let Ok(event) = events.try_recv() {
            if let PlatformEvent::Captured(c) = event {
                captured = Some(c)
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let captured = captured.expect("multi-format capture");
    assert!(captured.complete);
    assert_eq!(captured.offer.representations.len(), formats.len());
    service.publish(captured.offer.clone()).unwrap();
    for (target, bytes) in &formats {
        let name = String::from_utf8(conn.get_atom_name(*target)?.reply()?.name)?;
        let result = Command::new("xclip")
            .args(["-selection", "clipboard", "-o", "-target", &name])
            .output()?;
        assert!(result.status.success());
        assert_eq!(&result.stdout, bytes);
    }
    // Cooperative SAVE_TARGETS retains only the requested, supported target set.
    let manager = atom(b"CLIPBOARD_MANAGER");
    let save = atom(b"SAVE_TARGETS");
    let saved_prop = atom(b"TEST_SAVE_TARGETS");
    conn.set_selection_owner(source, clipboard, 0u32)?.check()?;
    conn.change_property32(
        PropMode::REPLACE,
        source,
        saved_prop,
        AtomEnum::ATOM,
        &[text, html],
    )?;
    conn.convert_selection(source, manager, save, saved_prop, 0u32)?;
    conn.flush()?;
    let end = Instant::now() + Duration::from_secs(5);
    let mut saved = false;
    while Instant::now() < end && !saved {
        while let Some(event) = conn.poll_for_event()? {
            match event {
                x11rb::protocol::Event::SelectionRequest(e) => {
                    respond(&conn, e, targets, &formats, Some(private))?
                }
                x11rb::protocol::Event::SelectionNotify(e) if e.selection == manager => {
                    assert_eq!(e.property, saved_prop);
                    saved = true;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(saved, "SAVE_TARGETS did not complete");
    let retained = conn
        .get_property(false, source, saved_prop, AtomEnum::ATOM, 0, 16)?
        .reply()?;
    let mut retained: Vec<_> = retained.value32().unwrap().collect();
    retained.sort_unstable();
    let mut expected = vec![text, html];
    expected.sort_unstable();
    assert_eq!(retained, expected);
    conn.destroy_window(source)?.check()?;
    wait_text("Unicode café 世界".as_bytes());
    // A focus change while a held modifier delays paste must never inject into the new window.
    while events.try_recv().is_ok() {}
    let intended = conn.generate_id()?;
    let unrelated = conn.generate_id()?;
    for window in [intended, unrelated] {
        conn.create_window(
            0,
            window,
            root,
            0,
            0,
            80,
            80,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )?
        .check()?;
        conn.map_window(window)?.check()?;
    }
    conn.set_input_focus(InputFocus::PARENT, unrelated, 0u32)?
        .check()?;
    let min = conn.setup().min_keycode;
    let mapping = conn
        .get_keyboard_mapping(min, conn.setup().max_keycode - min + 1)?
        .reply()?;
    let resolve = |symbol| {
        mapping
            .keysyms
            .chunks(mapping.keysyms_per_keycode as usize)
            .position(|syms| syms.first() == Some(&symbol))
            .map(|i| min + i as u8)
            .unwrap()
    };
    let ctrl = resolve(0xffe3);
    let v = resolve(0x76);
    use x11rb::protocol::xtest::ConnectionExt as _;
    conn.xtest_fake_input(KEY_PRESS_EVENT, ctrl, 0, root, 0, 0, 0)?
        .check()?;
    service
        .paste(
            Arc::new(Offer::text("safe clipboard fallback")),
            PasteTarget {
                window: intended,
                timestamp: 0,
            },
            PasteShortcut::ControlV,
        )
        .unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while conn.get_input_focus()?.reply()?.focus != intended {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
    conn.set_input_focus(InputFocus::PARENT, unrelated, 0u32)?
        .check()?;
    conn.xtest_fake_input(KEY_RELEASE_EVENT, ctrl, 0, root, 0, 0, 0)?
        .check()?;
    conn.flush()?;
    let end = Instant::now() + Duration::from_secs(2);
    let mut fallback = false;
    while Instant::now() < end && !fallback {
        if let Ok(PlatformEvent::PasteResult(status)) =
            events.recv_timeout(Duration::from_millis(20))
        {
            assert!(status.starts_with("Copied;"));
            fallback = true;
        }
    }
    assert!(fallback, "focus failure was not reported");
    while let Some(event) = conn.poll_for_event()? {
        if let x11rb::protocol::Event::KeyPress(key) = event {
            assert_ne!(
                key.detail, v,
                "paste was injected after the target lost focus"
            );
        }
    }
    wait_text(b"safe clipboard fallback");
    service.shutdown();
    Ok(())
}

fn respond(
    conn: &x11rb::rust_connection::RustConnection,
    e: SelectionRequestEvent,
    targets: Atom,
    formats: &[(Atom, Vec<u8>)],
    private: Option<Atom>,
) -> Result<()> {
    use x11rb::wrapper::ConnectionExt as _;
    if e.target == targets {
        let mut offered: Vec<_> = formats.iter().map(|(a, _)| *a).collect();
        offered.push(targets);
        offered.extend(private);
        conn.change_property32(
            PropMode::REPLACE,
            e.requestor,
            e.property,
            AtomEnum::ATOM,
            &offered,
        )?;
    } else if let Some((_, bytes)) = formats.iter().find(|(a, _)| *a == e.target) {
        conn.change_property8(PropMode::REPLACE, e.requestor, e.property, e.target, bytes)?;
    } else {
        panic!("unconfigured/private target requested")
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
    Ok(())
}
