//! Runtime wiring shared by the daemon and CLI: resolved paths (driven by
//! environment variables the NixOS module sets) and the status report type that
//! travels over the control socket.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    ARBITER_ENV_FILE, DEFAULT_CONFIG_PATH, DEFAULT_RUNTIME_DIR, SOCKET_FILE, SWAP_CONFIG_FILE,
};

/// Resolved runtime locations. Values are taken from the environment (set by the
/// systemd units in the NixOS module) and fall back to sensible defaults so the
/// binaries also work when invoked by hand.
#[derive(Debug, Clone)]
pub struct Runtime {
    pub config_path: PathBuf,
    pub runtime_dir: PathBuf,
    /// Absolute path to the `llama-server` binary (from the llama.cpp package).
    pub llama_server: String,
    /// The systemd unit name of the arbiter, restarted when config changes.
    pub arbiter_unit: String,
}

impl Runtime {
    /// Build a [`Runtime`] from the process environment.
    pub fn from_env() -> Self {
        let config_path = std::env::var("LOCAL_LLM_CONFIG")
            .unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string())
            .into();
        let runtime_dir = std::env::var("LOCAL_LLM_RUNTIME_DIR")
            .unwrap_or_else(|_| DEFAULT_RUNTIME_DIR.to_string())
            .into();
        let llama_server =
            std::env::var("LOCAL_LLM_LLAMA_SERVER").unwrap_or_else(|_| "llama-server".to_string());
        let arbiter_unit = std::env::var("LOCAL_LLM_ARBITER_UNIT")
            .unwrap_or_else(|_| "local-llm-arbiter.service".to_string());
        Self {
            config_path,
            runtime_dir,
            llama_server,
            arbiter_unit,
        }
    }

    pub fn swap_config_path(&self) -> PathBuf {
        self.runtime_dir.join(SWAP_CONFIG_FILE)
    }

    pub fn arbiter_env_path(&self) -> PathBuf {
        self.runtime_dir.join(ARBITER_ENV_FILE)
    }

    pub fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join(SOCKET_FILE)
    }
}

/// A point-in-time view of the system, returned by resourced over the socket.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusReport {
    pub config_path: String,
    pub config_valid: bool,
    pub config_error: Option<String>,
    pub listen: String,
    pub models: Vec<ModelStatus>,
    pub groups: Vec<String>,
    pub gpus: Vec<GpuStatus>,
    pub ram: Option<MemStatus>,
    /// Names of models llama-swap currently reports as running (best effort).
    pub running: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub name: String,
    pub source: String,
    pub aliases: Vec<String>,
    pub ttl: u32,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuStatus {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    pub memory_total_mb: u64,
    pub memory_used_mb: u64,
    pub utilization_pct: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemStatus {
    pub total_mb: u64,
    pub used_mb: u64,
}

/// Requests the CLI can send to resourced over the control socket. One JSON
/// object per line; resourced replies with one JSON line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Return the current [`StatusReport`].
    Status,
    /// Re-read config, regenerate the arbiter config and restart it.
    Reload,
    /// Validate the on-disk config without applying it.
    Validate,
}

/// Replies resourced sends back to the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Status(StatusReport),
    Ok { message: String },
    Error { message: String },
}
