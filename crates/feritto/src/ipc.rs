use crate::{ContextData, Envelope};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
const MAX_REQUEST: u64 = 34 * 1024 * 1024;
pub struct Instance {
    pub listener: UnixListener,
    socket: PathBuf,
    _lock: File,
}
impl Instance {
    pub fn claim(runtime: &Path, socket: &Path) -> Result<Option<Self>> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(runtime.join("service.lock"))?;
        match lock.try_lock_exclusive() {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        if socket.exists() {
            fs::remove_file(socket)?;
        }
        let listener = UnixListener::bind(socket)?;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
        Ok(Some(Self {
            listener,
            socket: socket.to_path_buf(),
            _lock: lock,
        }))
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}
pub fn forward(socket: &Path, request: &Envelope) -> Result<Option<Value>> {
    let mut stream = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream)
        .take(MAX_REQUEST)
        .read_line(&mut line)?;
    Ok(Some(
        serde_json::from_str(&line).context("invalid response from the running instance")?,
    ))
}
pub fn serve(listener: UnixListener, context: ContextData) -> Result<()> {
    thread::Builder::new()
        .name("feritto-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let result = (|| -> Result<Value> {
                    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
                    let mut line = String::new();
                    BufReader::new(&stream)
                        .take(MAX_REQUEST + 1)
                        .read_line(&mut line)?;
                    if line.len() as u64 > MAX_REQUEST || !line.ends_with('\n') {
                        bail!("IPC request is too large or incomplete")
                    }
                    context.handle_envelope(serde_json::from_str(&line)?)
                })();
                let reply = match result {
                    Ok(v) => v,
                    Err(error) => json!({"error":format!("{error:#}")}),
                };
                let _ = serde_json::to_writer(&mut stream, &reply);
                let _ = stream.write_all(b"\n");
            }
        })?;
    Ok(())
}
