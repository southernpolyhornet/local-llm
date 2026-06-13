//! Shared daemon state and the regenerate/restart logic.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result};
use local_llm_config::runtime::{ModelStatus, StatusReport};
use local_llm_config::{swap, Config, Runtime};

use crate::log;

pub struct SharedState {
    pub rt: Runtime,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    config: Option<Config>,
    config_error: Option<String>,
    last_yaml: Option<String>,
    gpus: Vec<local_llm_config::runtime::GpuStatus>,
    ram: Option<local_llm_config::runtime::MemStatus>,
}

impl SharedState {
    pub fn new(rt: Runtime) -> Self {
        Self {
            rt,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Re-read config from disk, regenerate artifacts and write them.
    /// Returns Ok(true) when the generated llama-swap config changed.
    pub fn regenerate(&self) -> Result<bool> {
        let cfg = match Config::load(&self.rt.config_path) {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("{e}");
                let mut inner = self.inner.lock().unwrap();
                inner.config_error = Some(msg.clone());
                anyhow::bail!(msg);
            }
        };

        let generated = swap::generate(&cfg, &self.rt);

        let changed = {
            let inner = self.inner.lock().unwrap();
            inner.last_yaml.as_deref() != Some(generated.yaml.as_str())
        };

        write_atomic(&self.rt.swap_config_path(), &generated.yaml)
            .context("writing llama-swap config")?;
        write_atomic(&self.rt.arbiter_env_path(), &generated.env)
            .context("writing arbiter env file")?;

        let mut inner = self.inner.lock().unwrap();
        inner.config = Some(cfg);
        inner.config_error = None;
        inner.last_yaml = Some(generated.yaml);
        Ok(changed)
    }

    /// Restart the arbiter unit via systemd (best effort).
    pub fn restart_arbiter(&self) {
        let unit = &self.rt.arbiter_unit;
        match Command::new("systemctl").arg("restart").arg(unit).status() {
            Ok(s) if s.success() => log(&format!("restarted {unit}")),
            Ok(s) => log(&format!("systemctl restart {unit} exited with {s}")),
            Err(e) => log(&format!("failed to run systemctl restart {unit}: {e}")),
        }
    }

    pub fn set_samples(
        &self,
        gpus: Vec<local_llm_config::runtime::GpuStatus>,
        ram: Option<local_llm_config::runtime::MemStatus>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        inner.gpus = gpus;
        inner.ram = ram;
    }

    pub fn sample_interval(&self) -> u64 {
        let inner = self.inner.lock().unwrap();
        inner
            .config
            .as_ref()
            .map(|c| c.resources.sample_interval.max(1))
            .unwrap_or(10)
    }

    pub fn listen(&self) -> String {
        let inner = self.inner.lock().unwrap();
        inner
            .config
            .as_ref()
            .map(|c| c.server.listen.clone())
            .unwrap_or_else(|| "127.0.0.1:8080".into())
    }

    /// Build a full status report for the CLI.
    pub fn status(&self) -> StatusReport {
        let running = crate::monitor::query_running(&self.listen());

        let inner = self.inner.lock().unwrap();
        let mut report = StatusReport {
            config_path: self.rt.config_path.display().to_string(),
            config_valid: inner.config.is_some() && inner.config_error.is_none(),
            config_error: inner.config_error.clone(),
            listen: inner
                .config
                .as_ref()
                .map(|c| c.server.listen.clone())
                .unwrap_or_default(),
            models: Vec::new(),
            groups: Vec::new(),
            gpus: inner.gpus.clone(),
            ram: inner.ram.clone(),
            running: running.clone(),
        };

        if let Some(cfg) = &inner.config {
            for (name, source, aliases, ttl) in swap::model_summaries(cfg) {
                let is_running = running.iter().any(|r| r == &name)
                    || aliases.iter().any(|a| running.contains(a));
                report.models.push(ModelStatus {
                    name,
                    source,
                    aliases,
                    ttl,
                    running: is_running,
                });
            }
            report.groups = cfg.groups.iter().map(|g| g.name.clone()).collect();
        }

        report
    }

    /// Validate the on-disk config without applying it.
    pub fn validate(&self) -> Result<()> {
        Config::load(&self.rt.config_path)
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// Write `contents` to `path` atomically (write temp + rename) with mode 0644.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.flush()?;
        let mut perms = f.metadata()?.permissions();
        perms.set_mode(0o644);
        f.set_permissions(perms)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}
