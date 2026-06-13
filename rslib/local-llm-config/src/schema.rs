//! The `config.toml` schema and validation.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Errors produced while loading or validating a configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse TOML in {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Top-level configuration, mirroring `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub storage: Storage,
    pub server: Server,
    pub resources: Resources,
    pub defaults: Defaults,
    #[serde(default, rename = "groups")]
    pub groups: Vec<Group>,
    #[serde(default, rename = "models")]
    pub models: Vec<Model>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Storage {
    pub models_dir: String,
    pub hf_cache_dir: String,
    pub auto_download: bool,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            models_dir: "/var/lib/local-llm/models".into(),
            hf_cache_dir: "/var/lib/local-llm/hf".into(),
            auto_download: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Server {
    pub listen: String,
    pub metrics: bool,
    pub health_check_timeout: u32,
    pub log_level: String,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".into(),
            metrics: true,
            health_check_timeout: 120,
            log_level: "info".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Resources {
    pub vram_budget_mb: u64,
    pub ram_budget_mb: u64,
    pub sample_interval: u64,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            vram_budget_mb: 0,
            ram_budget_mb: 0,
            sample_interval: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    pub ttl: u32,
    pub context_size: u32,
    pub flash_attention: bool,
    pub gpu_layers: u32,
    pub extra_args: Vec<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            ttl: 300,
            context_size: 8192,
            flash_attention: true,
            gpu_layers: 99,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub members: Vec<String>,
    /// When true only one member runs at a time (swap within the group).
    /// Defaults to false so grouped models stay resident together.
    #[serde(default)]
    pub swap: bool,
    /// When true, loading a member unloads models belonging to other groups.
    #[serde(default)]
    pub exclusive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Hugging Face repo, e.g. "Qwen/Qwen2.5-Coder-32B-Instruct-GGUF".
    #[serde(default)]
    pub hf_repo: Option<String>,
    /// Specific GGUF file within the repo (optional; llama.cpp can infer).
    #[serde(default)]
    pub hf_file: Option<String>,
    /// Local GGUF path; relative paths resolve against `storage.models_dir`.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub context_size: Option<u32>,
    #[serde(default)]
    pub ttl: Option<u32>,
    #[serde(default)]
    pub gpu_layers: Option<u32>,
    #[serde(default)]
    pub flash_attention: Option<bool>,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Max simultaneous in-flight requests for this model (0 = unlimited).
    #[serde(default)]
    pub concurrency_limit: u32,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl Config {
    /// Parse a configuration from a TOML string.
    pub fn from_str(s: &str, path: &str) -> Result<Self, ConfigError> {
        let cfg: Config = toml::from_str(s).map_err(|source| ConfigError::Parse {
            path: path.to_string(),
            source,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load and validate a configuration from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let display = path.display().to_string();
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: display.clone(),
            source,
        })?;
        Self::from_str(&raw, &display)
    }

    /// Validate semantic invariants beyond what the type system enforces.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.models.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one [[models]] entry is required".into(),
            ));
        }

        // Collect every routable name (model names + aliases) and ensure uniqueness.
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut model_names: BTreeSet<&str> = BTreeSet::new();
        for m in &self.models {
            if m.name.trim().is_empty() {
                return Err(ConfigError::Invalid("a model has an empty name".into()));
            }
            if !model_names.insert(m.name.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate model name: {}",
                    m.name
                )));
            }
            if m.hf_repo.is_none() && m.path.is_none() {
                return Err(ConfigError::Invalid(format!(
                    "model '{}' must set either `hf_repo` or `path`",
                    m.name
                )));
            }
            if m.hf_repo.is_some() && m.path.is_some() {
                return Err(ConfigError::Invalid(format!(
                    "model '{}' sets both `hf_repo` and `path`; choose one",
                    m.name
                )));
            }
        }
        for m in &self.models {
            for routable in std::iter::once(&m.name).chain(m.aliases.iter()) {
                if !seen.insert(routable.as_str()) {
                    return Err(ConfigError::Invalid(format!(
                        "name/alias '{}' is used more than once",
                        routable
                    )));
                }
            }
        }

        // Validate group membership references real models.
        let mut group_names: BTreeSet<&str> = BTreeSet::new();
        for g in &self.groups {
            if !group_names.insert(g.name.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate group name: {}",
                    g.name
                )));
            }
            for member in &g.members {
                if !model_names.contains(member.as_str()) {
                    return Err(ConfigError::Invalid(format!(
                        "group '{}' references unknown model '{}'",
                        g.name, member
                    )));
                }
            }
        }

        if self.server.listen.parse::<std::net::SocketAddr>().is_err()
            && !self.server.listen.starts_with(':')
        {
            return Err(ConfigError::Invalid(format!(
                "server.listen '{}' is not a valid host:port",
                self.server.listen
            )));
        }

        Ok(())
    }

    /// Effective TTL for a model, falling back to the global default.
    pub fn model_ttl(&self, m: &Model) -> u32 {
        m.ttl.unwrap_or(self.defaults.ttl)
    }

    /// Effective context size for a model.
    pub fn model_context(&self, m: &Model) -> u32 {
        m.context_size.unwrap_or(self.defaults.context_size)
    }

    /// Effective GPU layer count for a model.
    pub fn model_gpu_layers(&self, m: &Model) -> u32 {
        m.gpu_layers.unwrap_or(self.defaults.gpu_layers)
    }

    /// Effective flash-attention setting for a model.
    pub fn model_flash_attention(&self, m: &Model) -> bool {
        m.flash_attention.unwrap_or(self.defaults.flash_attention)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[storage]
models_dir = "/tmp/models"

[[models]]
name = "chat"
hf_repo = "Qwen/Qwen2.5-Coder-32B-Instruct-GGUF"
aliases = ["gpt-4o"]

[[models]]
name = "autocomplete"
path = "tiny.gguf"

[[groups]]
name = "coding"
members = ["chat", "autocomplete"]
"#;

    #[test]
    fn parses_valid_config() {
        let cfg = Config::from_str(SAMPLE, "test").unwrap();
        assert_eq!(cfg.models.len(), 2);
        assert_eq!(cfg.groups.len(), 1);
        assert_eq!(cfg.storage.models_dir, "/tmp/models");
        // Defaults fill in.
        assert_eq!(cfg.defaults.ttl, 300);
    }

    #[test]
    fn rejects_empty_models() {
        let err = Config::from_str("[storage]\nmodels_dir=\"/x\"\n", "test").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn rejects_duplicate_alias() {
        let toml = r#"
[[models]]
name = "a"
path = "a.gguf"
aliases = ["dup"]

[[models]]
name = "dup"
path = "b.gguf"
"#;
        let err = Config::from_str(toml, "test").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn rejects_unknown_group_member() {
        let toml = r#"
[[models]]
name = "a"
path = "a.gguf"

[[groups]]
name = "g"
members = ["missing"]
"#;
        let err = Config::from_str(toml, "test").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn rejects_model_without_source() {
        let toml = r#"
[[models]]
name = "a"
"#;
        let err = Config::from_str(toml, "test").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }
}
