//! `local-llm`: the operator CLI for the local-llm stack.

mod client;

use std::io::{IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use local_llm_config::runtime::{Request, Response};
use local_llm_config::{Config, Runtime};

const RESOURCED_UNIT: &str = "local-llm-resourced.service";
const ARBITER_UNIT: &str = "local-llm-arbiter.service";

#[derive(Parser)]
#[command(
    name = "local-llm",
    version,
    about = "Manage the local LLM stack (llama-swap + llama.cpp) on NixOS"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the configuration in your editor (nano by default), then validate.
    Configure,
    /// Show service, model and resource status.
    Status {
        /// Emit the raw status report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Validate the configuration without applying it.
    Validate,
    /// Apply config changes: regenerate the arbiter config and restart it.
    Reload,
    /// Start both local-llm services.
    Start,
    /// Stop both local-llm services.
    Stop,
    /// Restart both local-llm services.
    Restart,
    /// Tail the service logs.
    Logs {
        /// Follow the logs (like `journalctl -f`).
        #[arg(short, long)]
        follow: bool,
        /// Which service to show.
        #[arg(long, value_enum, default_value_t = LogTarget::All)]
        unit: LogTarget,
    },
    /// Inspect configured models.
    Models {
        #[command(subcommand)]
        action: Option<ModelsAction>,
    },
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum LogTarget {
    All,
    Arbiter,
    Resourced,
}

#[derive(Subcommand)]
enum ModelsAction {
    /// List configured models (default).
    List,
    /// Show which models are currently loaded.
    Running,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let rt = Runtime::from_env();
    match cli.command {
        Cmd::Configure => configure(&rt),
        Cmd::Status { json } => status(&rt, json),
        Cmd::Validate => validate(&rt),
        Cmd::Reload => reload(&rt),
        Cmd::Start => systemctl(&["start", RESOURCED_UNIT, ARBITER_UNIT]),
        Cmd::Stop => systemctl(&["stop", ARBITER_UNIT, RESOURCED_UNIT]),
        Cmd::Restart => systemctl(&["restart", RESOURCED_UNIT, ARBITER_UNIT]),
        Cmd::Logs { follow, unit } => logs(follow, unit),
        Cmd::Models { action } => models(&rt, action.unwrap_or(ModelsAction::List)),
    }
}

/// `configure`: open the config in nano/$EDITOR, then validate and offer reload.
fn configure(rt: &Runtime) -> Result<()> {
    let path = rt.config_path.clone();
    ensure_writable_or_sudo(&path)?;

    if !path.exists() {
        anyhow::bail!(
            "config not found at {} (is the local-llm NixOS module enabled?)",
            path.display()
        );
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "nano".to_string());

    let status = Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("launching editor '{editor}'"))?;
    if !status.success() {
        eprintln!("editor exited without success; leaving config unchanged");
        return Ok(());
    }

    // Validate after editing.
    match Config::load(&path) {
        Ok(_) => println!("configuration is valid."),
        Err(e) => {
            eprintln!("warning: configuration is INVALID: {e}");
            eprintln!("the arbiter will keep using the last valid config until fixed.");
            return Ok(());
        }
    }

    if std::io::stdin().is_terminal() && prompt_yes_no("Apply changes now (reload)?", true)? {
        reload(rt)?;
    } else {
        println!("Run `local-llm reload` when you're ready to apply changes.");
    }
    Ok(())
}

fn status(rt: &Runtime, json: bool) -> Result<()> {
    let resp = client::request(rt, &Request::Status)?;
    let report = match resp {
        Response::Status(s) => s,
        Response::Error { message } => anyhow::bail!(message),
        Response::Ok { message } => anyhow::bail!("unexpected response: {message}"),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("Config:   {}", report.config_path);
    if report.config_valid {
        println!("Status:   valid");
    } else {
        println!(
            "Status:   INVALID{}",
            report
                .config_error
                .as_ref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        );
    }
    if !report.listen.is_empty() {
        println!(
            "Endpoint: http://{}/v1 (OpenAI), http://{}/ (Anthropic)",
            report.listen, report.listen
        );
    }

    println!("\nModels:");
    if report.models.is_empty() {
        println!("  (none configured)");
    }
    for m in &report.models {
        let mark = if m.running { "*" } else { " " };
        let aliases = if m.aliases.is_empty() {
            String::new()
        } else {
            format!("  aliases: {}", m.aliases.join(", "))
        };
        println!(
            "  [{mark}] {:<16} ttl={}s  {}{}",
            m.name, m.ttl, m.source, aliases
        );
    }
    if !report.groups.is_empty() {
        println!("\nGroups:   {}", report.groups.join(", "));
    }

    println!("\nGPUs:");
    if report.gpus.is_empty() {
        println!("  (none detected; CPU mode or no GPU tools available)");
    }
    for g in &report.gpus {
        let util = g
            .utilization_pct
            .map(|u| format!("{u}%"))
            .unwrap_or_else(|| "?".into());
        println!(
            "  [{}] {} ({}): {} / {} MB VRAM, util {}",
            g.index, g.name, g.vendor, g.memory_used_mb, g.memory_total_mb, util
        );
    }

    if let Some(ram) = &report.ram {
        println!("\nRAM:      {} / {} MB", ram.used_mb, ram.total_mb);
    }
    let _ = std::io::stdout().flush();
    Ok(())
}

fn validate(rt: &Runtime) -> Result<()> {
    // Prefer the daemon (authoritative), but fall back to local parsing.
    match client::request(rt, &Request::Validate) {
        Ok(Response::Ok { message }) => {
            println!("{message}");
            Ok(())
        }
        Ok(Response::Error { message }) => anyhow::bail!(message),
        Ok(_) => anyhow::bail!("unexpected response from resourced"),
        Err(_) => {
            Config::load(&rt.config_path).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("configuration is valid.");
            Ok(())
        }
    }
}

fn reload(rt: &Runtime) -> Result<()> {
    match client::request(rt, &Request::Reload) {
        Ok(Response::Ok { message }) => {
            println!("{message}");
            Ok(())
        }
        Ok(Response::Error { message }) => anyhow::bail!(message),
        Ok(_) => anyhow::bail!("unexpected response from resourced"),
        Err(e) => {
            eprintln!("could not reach resourced ({e}); falling back to systemctl");
            systemctl(&["restart", RESOURCED_UNIT, ARBITER_UNIT])
        }
    }
}

fn models(rt: &Runtime, action: ModelsAction) -> Result<()> {
    match action {
        ModelsAction::List => {
            let cfg = Config::load(&rt.config_path).map_err(|e| anyhow::anyhow!("{e}"))?;
            for m in &cfg.models {
                let source = m
                    .hf_repo
                    .clone()
                    .or_else(|| m.path.clone())
                    .unwrap_or_else(|| "<none>".into());
                println!("{:<16} {}", m.name, source);
                if !m.aliases.is_empty() {
                    println!("    aliases: {}", m.aliases.join(", "));
                }
            }
            println!("\nEdit models with `local-llm configure`.");
            Ok(())
        }
        ModelsAction::Running => {
            let resp = client::request(rt, &Request::Status)?;
            if let Response::Status(s) = resp {
                if s.running.is_empty() {
                    println!("(no models currently loaded)");
                }
                for r in s.running {
                    println!("{r}");
                }
            }
            Ok(())
        }
    }
}

fn logs(follow: bool, unit: LogTarget) -> Result<()> {
    let mut args = vec!["--no-pager".to_string()];
    match unit {
        LogTarget::All => {
            args.push(format!("--unit={RESOURCED_UNIT}"));
            args.push(format!("--unit={ARBITER_UNIT}"));
        }
        LogTarget::Arbiter => args.push(format!("--unit={ARBITER_UNIT}")),
        LogTarget::Resourced => args.push(format!("--unit={RESOURCED_UNIT}")),
    }
    if follow {
        args.push("-f".into());
    } else {
        args.push("-n".into());
        args.push("100".into());
    }
    let err = Command::new("journalctl").args(&args).exec();
    Err(err).context("running journalctl")
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .context("running systemctl")?;
    if !status.success() {
        anyhow::bail!("systemctl {} failed", args.join(" "));
    }
    Ok(())
}

/// Re-exec under sudo if the config path is not writable by the current user.
fn ensure_writable_or_sudo(path: &Path) -> Result<()> {
    if can_write(path) {
        return Ok(());
    }
    eprintln!(
        "Editing {} requires root; re-running under sudo...",
        path.display()
    );
    let exe = std::env::current_exe().context("resolving own path")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let err = Command::new("sudo")
        .arg("--preserve-env=EDITOR,VISUAL,LOCAL_LLM_CONFIG,LOCAL_LLM_RUNTIME_DIR")
        .arg(exe)
        .args(&args)
        .exec();
    Err(err).context("escalating with sudo")
}

/// Best-effort writability check: existing file openable for write, or parent
/// directory writable when the file does not yet exist.
fn can_write(path: &Path) -> bool {
    if path.exists() {
        std::fs::OpenOptions::new().write(true).open(path).is_ok()
    } else if let Some(dir) = path.parent() {
        let probe = dir.join(".local-llm-write-test");
        match std::fs::File::create(&probe) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(_) => false,
        }
    } else {
        false
    }
}

fn prompt_yes_no(question: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("{question} {hint} ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let answer = line.trim().to_lowercase();
    Ok(match answer.as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    })
}
