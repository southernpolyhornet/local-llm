//! Unix control socket: one JSON request line in, one JSON response line out.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;

use anyhow::{Context, Result};
use local_llm_config::runtime::{Request, Response};

use crate::log;
use crate::state::SharedState;

pub fn serve(state: Arc<SharedState>) -> Result<()> {
    let path = state.rt.socket_path();
    // Remove any stale socket from a previous run.
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding control socket {}", path.display()))?;

    // Allow unprivileged local users to query status / request reloads.
    if let Ok(meta) = std::fs::metadata(&path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o666);
        let _ = std::fs::set_permissions(&path, perms);
    }
    log(&format!("control socket listening at {}", path.display()));

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    if let Err(e) = handle(stream, state) {
                        log(&format!("client error: {e:#}"));
                    }
                });
            }
            Err(e) => log(&format!("accept error: {e}")),
        }
    }
    Ok(())
}

fn handle(stream: UnixStream, state: Arc<SharedState>) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    let response = match serde_json::from_str::<Request>(line) {
        Ok(Request::Status) => Response::Status(state.status()),
        Ok(Request::Validate) => match state.validate() {
            Ok(()) => Response::Ok {
                message: "configuration is valid".into(),
            },
            Err(e) => Response::Error {
                message: format!("{e}"),
            },
        },
        Ok(Request::Reload) => match state.regenerate() {
            Ok(_) => {
                state.restart_arbiter();
                Response::Ok {
                    message: "reloaded; arbiter restarting".into(),
                }
            }
            Err(e) => Response::Error {
                message: format!("{e}"),
            },
        },
        Err(e) => Response::Error {
            message: format!("invalid request: {e}"),
        },
    };

    let mut stream = stream;
    let mut payload = serde_json::to_string(&response)?;
    payload.push('\n');
    stream.write_all(payload.as_bytes())?;
    stream.flush()?;
    Ok(())
}
