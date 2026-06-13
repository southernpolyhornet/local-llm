//! local-llm-resourced: the resource-manager daemon.
//!
//! Responsibilities (thin orchestration; llama-swap does the actual swapping):
//!   * Watch `/etc/local-llm/config.toml` and, on change, regenerate the
//!     llama-swap YAML + arbiter env file and restart the arbiter.
//!   * Sample GPU/RAM utilization for `local-llm status`.
//!   * Serve status/reload/validate requests on a unix control socket.

mod monitor;
mod server;
mod state;

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use local_llm_config::Runtime;
use notify::{RecursiveMode, Watcher};

use crate::state::SharedState;

fn main() -> Result<()> {
    let rt = Runtime::from_env();
    log(&format!(
        "starting; config={} runtime_dir={}",
        rt.config_path.display(),
        rt.runtime_dir.display()
    ));

    std::fs::create_dir_all(&rt.runtime_dir)
        .with_context(|| format!("creating runtime dir {}", rt.runtime_dir.display()))?;

    let state = Arc::new(SharedState::new(rt.clone()));

    // Initial generation so the arbiter has a config before it starts.
    match state.regenerate() {
        Ok(_) => log("generated initial arbiter config"),
        Err(e) => log(&format!("WARNING: initial config invalid: {e:#}")),
    }

    // Tell systemd we're ready (Type=notify). The arbiter is ordered after us.
    if let Err(e) = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]) {
        log(&format!("sd_notify failed (non-fatal): {e}"));
    }

    // Resource sampler thread.
    {
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("sampler".into())
            .spawn(move || monitor::sampler_loop(state))
            .context("spawning sampler thread")?;
    }

    // Control socket thread.
    {
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("socket".into())
            .spawn(move || {
                if let Err(e) = server::serve(state) {
                    log(&format!("control socket exited: {e:#}"));
                }
            })
            .context("spawning socket thread")?;
    }

    // File watcher on the parent directory (editors replace the file via rename,
    // so watching the directory is more reliable than watching the file inode).
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("creating file watcher")?;

    let watch_dir = rt
        .config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/etc/local-llm"));
    if let Err(e) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
        log(&format!(
            "WARNING: cannot watch {} ({e}); live reload disabled",
            watch_dir.display()
        ));
    } else {
        log(&format!("watching {} for changes", watch_dir.display()));
    }

    let config_name = rt.config_path.file_name().map(|s| s.to_owned());
    while let Ok(first) = rx.recv() {
        // Block for the next event, then debounce by draining for a moment.
        let mut relevant = event_touches(&first, config_name.as_deref());
        let deadline = std::time::Instant::now() + Duration::from_millis(400);
        while let Ok(remaining) = rx.recv_timeout(
            deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::from_secs(0)),
        ) {
            relevant |= event_touches(&remaining, config_name.as_deref());
        }

        if !relevant {
            continue;
        }

        log("config change detected; regenerating");
        match state.regenerate() {
            Ok(true) => {
                log("config changed; restarting arbiter");
                state.restart_arbiter();
            }
            Ok(false) => log("config unchanged after regeneration"),
            Err(e) => log(&format!("config invalid, keeping previous: {e:#}")),
        }
    }

    Ok(())
}

/// Whether a watcher event concerns our config file.
fn event_touches(
    res: &notify::Result<notify::Event>,
    config_name: Option<&std::ffi::OsStr>,
) -> bool {
    let Ok(event) = res else {
        return false;
    };
    match config_name {
        None => true,
        Some(name) => event.paths.iter().any(|p| p.file_name() == Some(name)),
    }
}

/// Minimal timestamp-free logging to stderr (journald adds timestamps).
pub fn log(msg: &str) {
    eprintln!("[resourced] {msg}");
}
