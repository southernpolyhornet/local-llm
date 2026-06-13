//! Translation from our `config.toml` into llama-swap's YAML config plus the
//! systemd environment file the arbiter is launched with.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::runtime::Runtime;
use crate::schema::{Config, Model};

/// llama-swap top-level configuration document.
#[derive(Debug, Serialize)]
struct SwapConfig {
    #[serde(rename = "healthCheckTimeout")]
    health_check_timeout: u32,
    #[serde(rename = "logLevel")]
    log_level: String,
    #[serde(rename = "startPort")]
    start_port: u32,
    #[serde(rename = "includeAliasesInList")]
    include_aliases_in_list: bool,
    models: BTreeMap<String, SwapModel>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    groups: BTreeMap<String, SwapGroup>,
}

#[derive(Debug, Serialize)]
struct SwapModel {
    cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    ttl: u32,
    #[serde(rename = "concurrencyLimit", skip_serializing_if = "is_zero")]
    concurrency_limit: u32,
}

#[derive(Debug, Serialize)]
struct SwapGroup {
    swap: bool,
    exclusive: bool,
    members: Vec<String>,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// Generated artifacts ready to be written to the runtime directory.
pub struct Generated {
    pub yaml: String,
    pub env: String,
}

/// Quote an argument for safe inclusion in a single shell command line.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '+')
        })
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// Resolve a possibly-relative local model path against the models directory.
fn resolve_model_path(models_dir: &str, path: &str) -> String {
    if Path::new(path).is_absolute() {
        path.to_string()
    } else {
        Path::new(models_dir)
            .join(path)
            .to_string_lossy()
            .into_owned()
    }
}

/// Build the `llama-server` command line for a single model.
fn build_cmd(cfg: &Config, rt: &Runtime, m: &Model) -> String {
    let mut args: Vec<String> = vec![
        shell_quote(&rt.llama_server),
        "--port".into(),
        "${PORT}".into(),
        "--host".into(),
        "127.0.0.1".into(),
    ];

    if let Some(repo) = &m.hf_repo {
        args.push("--hf-repo".into());
        args.push(shell_quote(repo));
        if let Some(file) = &m.hf_file {
            args.push("--hf-file".into());
            args.push(shell_quote(file));
        }
    } else if let Some(path) = &m.path {
        let resolved = resolve_model_path(&cfg.storage.models_dir, path);
        args.push("--model".into());
        args.push(shell_quote(&resolved));
    }

    args.push("--ctx-size".into());
    args.push(cfg.model_context(m).to_string());

    args.push("--n-gpu-layers".into());
    args.push(cfg.model_gpu_layers(m).to_string());

    if cfg.model_flash_attention(m) {
        args.push("--flash-attn".into());
        args.push("on".into());
    }

    for a in cfg.defaults.extra_args.iter().chain(m.extra_args.iter()) {
        args.push(shell_quote(a));
    }

    args.join(" ")
}

fn model_source(cfg: &Config, m: &Model) -> String {
    if let Some(repo) = &m.hf_repo {
        match &m.hf_file {
            Some(file) => format!("hf:{repo}/{file}"),
            None => format!("hf:{repo}"),
        }
    } else if let Some(path) = &m.path {
        resolve_model_path(&cfg.storage.models_dir, path)
    } else {
        "<none>".into()
    }
}

/// Generate the llama-swap YAML and the arbiter environment file.
pub fn generate(cfg: &Config, rt: &Runtime) -> Generated {
    let mut models = BTreeMap::new();
    for m in &cfg.models {
        models.insert(
            m.name.clone(),
            SwapModel {
                cmd: build_cmd(cfg, rt, m),
                name: Some(m.name.clone()),
                description: m.description.clone(),
                aliases: m.aliases.clone(),
                ttl: cfg.model_ttl(m),
                concurrency_limit: m.concurrency_limit,
            },
        );
    }

    let mut groups = BTreeMap::new();
    for g in &cfg.groups {
        groups.insert(
            g.name.clone(),
            SwapGroup {
                swap: g.swap,
                exclusive: g.exclusive,
                members: g.members.clone(),
            },
        );
    }

    let doc = SwapConfig {
        health_check_timeout: cfg.server.health_check_timeout,
        log_level: cfg.server.log_level.clone(),
        start_port: 9800,
        include_aliases_in_list: cfg.server.list_aliases,
        models,
        groups,
    };

    let yaml = serde_yaml::to_string(&doc).expect("serializing llama-swap config");
    let header = "# Generated by local-llm-resourced from /etc/local-llm/config.toml.\n# Do not edit by hand; run `local-llm configure` instead.\n";
    let yaml = format!("{header}{yaml}");

    let env = build_env(cfg);
    Generated { yaml, env }
}

/// Build the systemd EnvironmentFile contents for the arbiter.
fn build_env(cfg: &Config) -> String {
    let mut lines = vec![
        "# Generated by local-llm-resourced. Do not edit by hand.".to_string(),
        format!("LOCAL_LLM_LISTEN={}", cfg.server.listen),
        // llama.cpp downloads GGUFs into LLAMA_CACHE; keep models on the
        // user-chosen drive. HF_HOME holds the broader Hugging Face cache.
        format!("LLAMA_CACHE={}", cfg.storage.models_dir),
        format!("HF_HOME={}", cfg.storage.hf_cache_dir),
        format!("HF_HUB_CACHE={}", cfg.storage.hf_cache_dir),
    ];
    if !cfg.storage.auto_download {
        lines.push("HF_HUB_OFFLINE=1".to_string());
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Build a status summary of configured models (used by resourced).
pub fn model_summaries(cfg: &Config) -> Vec<(String, String, Vec<String>, u32)> {
    cfg.models
        .iter()
        .map(|m| {
            (
                m.name.clone(),
                model_source(cfg, m),
                m.aliases.clone(),
                cfg.model_ttl(m),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> Runtime {
        Runtime {
            config_path: "/etc/local-llm/config.toml".into(),
            runtime_dir: "/run/local-llm".into(),
            llama_server: "/nix/store/x/bin/llama-server".into(),
            arbiter_unit: "local-llm-arbiter.service".into(),
        }
    }

    #[test]
    fn generates_yaml_with_models_and_groups() {
        let cfg = Config::from_str(
            r#"
[storage]
models_dir = "/data/models"

[[models]]
name = "chat"
hf_repo = "Qwen/Qwen2.5-Coder-32B-Instruct-GGUF"
hf_file = "q4.gguf"
context_size = 32768
aliases = ["gpt-4o"]

[[models]]
name = "local"
path = "tiny.gguf"

[[groups]]
name = "coding"
members = ["chat", "local"]
"#,
            "test",
        )
        .unwrap();

        let gen = generate(&cfg, &rt());
        assert!(gen.yaml.contains("chat:"));
        assert!(gen
            .yaml
            .contains("--hf-repo Qwen/Qwen2.5-Coder-32B-Instruct-GGUF"));
        assert!(gen.yaml.contains("--hf-file q4.gguf"));
        assert!(gen.yaml.contains("--ctx-size 32768"));
        assert!(gen.yaml.contains("${PORT}"));
        assert!(gen.yaml.contains("/data/models/tiny.gguf"));
        assert!(gen.yaml.contains("coding:"));
        assert!(gen.yaml.contains("gpt-4o"));

        assert!(gen.env.contains("LLAMA_CACHE=/data/models"));
        assert!(gen.env.contains("LOCAL_LLM_LISTEN=127.0.0.1:8080"));
    }

    #[test]
    fn absolute_path_preserved() {
        assert_eq!(resolve_model_path("/data", "/abs/x.gguf"), "/abs/x.gguf");
        assert_eq!(resolve_model_path("/data", "rel.gguf"), "/data/rel.gguf");
    }
}
