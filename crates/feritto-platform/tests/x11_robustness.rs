//! Run with xvfb-run -a cargo test -p feritto-platform --test x11_robustness -- --ignored.
//! Failure paths that must never stop the X11 worker or strand a SAVE_TARGETS requestor.
use anyhow::Result;
use feritto_core::*;
use feritto_platform::*;
use std::{
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};
use x11rb::{
    NONE, connection::Connection, protocol::Event, protocol::xproto::*,
    rust_connection::RustConnection, wrapper::ConnectionExt as _,
};

fn window(conn: &RustConnection) -> Result<Window> {
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
    Ok(window)
}

fn reply(conn: &RustConnection, e: &SelectionRequestEvent, property: Atom) -> Result<()> {
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
            property,
        },
    )?;
    conn.flush()?;
    Ok(())
}

#[test]
#[ignore = "requires an isolated native X11 server and xclip"]
fn worker_survives_failed_transfers_and_answers_save_targets() -> Result<()> {
    let mut settings = Settings::default();
    settings.capture.progress_timeout_ms = 300;
    settings.capture.generation_timeout_ms = 1000;
    let (service, events) = X11Service::start(settings)?;
    let (conn, _) = x11rb::connect(None)?;
    let atom = |name: &[u8]| conn.intern_atom(false, name).unwrap().reply().unwrap().atom;
    let clipboard = atom(b"CLIPBOARD");
    let manager = atom(b"CLIPBOARD_MANAGER");
    let save = atom(b"SAVE_TARGETS");
    let targets = atom(b"TARGETS");
    let utf8 = atom(b"UTF8_STRING");
    let text = atom(b"TEXT");
    let save_prop = atom(b"TEST_SAVE");

    // 1. The source stalls on data while the SAVE_TARGETS requestor exits before the capture
    //    times out. Answering the vanished requestor must not stop the worker.
    let source = window(&conn)?;
    let requestor = window(&conn)?;
    conn.set_selection_owner(source, clipboard, 0u32)?.check()?;
    service.active_window().unwrap(); // barrier: the ownership capture is queued first
    conn.convert_selection(requestor, manager, save, NONE, 0u32)?;
    conn.flush()?;
    let end = Instant::now() + Duration::from_secs(3);
    let mut stalled = false;
    while Instant::now() < end && !stalled {
        match conn.poll_for_event()? {
            Some(Event::SelectionRequest(e)) if e.target == targets => {
                conn.change_property32(
                    PropMode::REPLACE,
                    e.requestor,
                    e.property,
                    AtomEnum::ATOM,
                    &[targets, utf8],
                )?;
                reply(&conn, &e, e.property)?;
            }
            // Never answer the data request: the capture must time out.
            Some(Event::SelectionRequest(e)) if e.target == utf8 => stalled = true,
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    assert!(stalled, "the service did not request data from the source");
    conn.destroy_window(requestor)?.check()?;
    std::thread::sleep(Duration::from_millis(1500));
    service
        .active_window()
        .expect("X11 worker stopped after answering a destroyed SAVE_TARGETS requestor");
    while events.try_recv().is_ok() {}

    // 2. A TARGETS list naming a nonexistent atom must not strand the SAVE_TARGETS requestor:
    //    unknown atoms are skipped and the supported text is still saved.
    let source = window(&conn)?;
    let requestor = window(&conn)?;
    conn.set_selection_owner(source, clipboard, 0u32)?.check()?;
    service.active_window().unwrap();
    conn.convert_selection(requestor, manager, save, save_prop, 0u32)?;
    conn.flush()?;
    let end = Instant::now() + Duration::from_secs(3);
    let mut answered = None;
    while Instant::now() < end && answered.is_none() {
        match conn.poll_for_event()? {
            Some(Event::SelectionRequest(e)) if e.target == targets => {
                conn.change_property32(
                    PropMode::REPLACE,
                    e.requestor,
                    e.property,
                    AtomEnum::ATOM,
                    &[targets, utf8, 0x1fff_fff0],
                )?;
                reply(&conn, &e, e.property)?;
            }
            Some(Event::SelectionRequest(e)) if e.target == utf8 => {
                conn.change_property8(PropMode::REPLACE, e.requestor, e.property, utf8, b"kept")?;
                reply(&conn, &e, e.property)?;
            }
            Some(Event::SelectionRequest(e)) => reply(&conn, &e, NONE)?,
            Some(Event::SelectionNotify(e)) if e.selection == manager => answered = Some(e),
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let answered = answered.expect("SAVE_TARGETS requestor was never answered");
    assert_eq!(answered.requestor, requestor);
    assert_eq!(
        answered.property, save_prop,
        "supported text should be saved"
    );
    conn.destroy_window(source)?.check()?;
    conn.destroy_window(requestor)?.check()?;
    conn.flush()?;

    // 3. TEXT is a request for "any text encoding"; ICCCM forbids answering with type TEXT.
    service
        .publish(Arc::new(Offer::text("typed text")))
        .unwrap();
    let probe = window(&conn)?;
    let probe_prop = atom(b"TEST_TEXT");
    conn.convert_selection(probe, clipboard, text, probe_prop, 0u32)?;
    conn.flush()?;
    loop {
        if let Event::SelectionNotify(e) = conn.wait_for_event()? {
            assert_eq!(e.property, probe_prop);
            break;
        }
    }
    let value = conn
        .get_property(false, probe, probe_prop, AtomEnum::ANY, 0, 64)?
        .reply()?;
    assert_eq!(
        value.type_, utf8,
        "TEXT must be answered with a concrete encoding"
    );
    assert_eq!(value.value, b"typed text");
    let out = Command::new("xclip")
        .args(["-selection", "clipboard", "-o", "-target", "TEXT"])
        .output()?;
    assert_eq!(out.stdout, b"typed text");
    service.shutdown();
    Ok(())
}
