//! Configuration model for local-llm.
//!
//! This crate owns the `/etc/local-llm/config.toml` schema, validates it, and
//! translates it into a [llama-swap](https://github.com/mostlygeek/llama-swap)
//! YAML configuration. It is shared by the `local-llm` CLI and the
//! `local-llm-resourced` daemon.

pub mod runtime;
pub mod schema;
pub mod swap;

pub use runtime::{Runtime, StatusReport};
pub use schema::{Config, ConfigError};

/// Default path to the mutable configuration file.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/local-llm/config.toml";

/// Default runtime directory where generated artifacts are written.
pub const DEFAULT_RUNTIME_DIR: &str = "/run/local-llm";

/// File name (within the runtime dir) of the generated llama-swap config.
pub const SWAP_CONFIG_FILE: &str = "llama-swap.yaml";

/// File name (within the runtime dir) of the generated arbiter environment file.
pub const ARBITER_ENV_FILE: &str = "arbiter.env";

/// File name (within the runtime dir) of the resourced control socket.
pub const SOCKET_FILE: &str = "resourced.sock";
