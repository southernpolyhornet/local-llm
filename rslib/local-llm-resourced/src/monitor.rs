//! Resource sampling: GPU (nvidia-smi / rocm-smi), system RAM, and a best-effort
//! query of llama-swap's `/running` endpoint.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use local_llm_config::runtime::{GpuStatus, MemStatus};

use crate::log;
use crate::state::SharedState;

/// Periodically sample resources into shared state.
pub fn sampler_loop(state: Arc<SharedState>) {
    loop {
        let gpus = sample_gpus();
        let ram = sample_ram();
        state.set_samples(gpus, ram);
        std::thread::sleep(Duration::from_secs(state.sample_interval()));
    }
}

/// Collect GPU stats, trying NVIDIA first then AMD ROCm. Returns empty on CPU.
pub fn sample_gpus() -> Vec<GpuStatus> {
    if let Some(g) = sample_nvidia() {
        if !g.is_empty() {
            return g;
        }
    }
    if let Some(g) = sample_rocm() {
        if !g.is_empty() {
            return g;
        }
    }
    Vec::new()
}

fn sample_nvidia() -> Option<Vec<GpuStatus>> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,name,memory.total,memory.used,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut gpus = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if cols.len() < 5 {
            continue;
        }
        gpus.push(GpuStatus {
            index: cols[0].parse().unwrap_or(0),
            name: cols[1].to_string(),
            vendor: "nvidia".into(),
            memory_total_mb: cols[2].parse().unwrap_or(0),
            memory_used_mb: cols[3].parse().unwrap_or(0),
            utilization_pct: cols[4].parse().ok(),
        });
    }
    Some(gpus)
}

fn sample_rocm() -> Option<Vec<GpuStatus>> {
    // rocm-smi JSON output varies across versions; parse defensively.
    let out = Command::new("rocm-smi")
        .args(["--showmeminfo", "vram", "--showuse", "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let obj = json.as_object()?;
    let mut gpus = Vec::new();
    for (key, card) in obj {
        if !key.starts_with("card") {
            continue;
        }
        let index = key.trim_start_matches("card").parse().unwrap_or(0);
        let total = find_number(card, &["VRAM Total Memory (B)", "VRAM Total Memory"])
            .map(|b| b / 1_048_576)
            .unwrap_or(0);
        let used = find_number(
            card,
            &["VRAM Total Used Memory (B)", "VRAM Total Used Memory"],
        )
        .map(|b| b / 1_048_576)
        .unwrap_or(0);
        let util = find_number(card, &["GPU use (%)"]).map(|v| v as u32);
        gpus.push(GpuStatus {
            index,
            name: card
                .get("Card series")
                .or_else(|| card.get("Card model"))
                .and_then(|v| v.as_str())
                .unwrap_or("AMD GPU")
                .to_string(),
            vendor: "amd".into(),
            memory_total_mb: total,
            memory_used_mb: used,
            utilization_pct: util,
        });
    }
    Some(gpus)
}

fn find_number(card: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    for k in keys {
        if let Some(v) = card.get(*k) {
            if let Some(n) = v.as_u64() {
                return Some(n);
            }
            if let Some(s) = v.as_str() {
                if let Ok(n) = s.trim().parse::<u64>() {
                    return Some(n);
                }
                if let Ok(f) = s.trim().parse::<f64>() {
                    return Some(f as u64);
                }
            }
        }
    }
    None
}

/// Read total/used system memory from /proc/meminfo (values in MB).
pub fn sample_ram() -> Option<MemStatus> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = 0u64;
    let mut avail_kb = 0u64;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("MemTotal:") => total_kb = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            Some("MemAvailable:") => {
                avail_kb = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0)
            }
            _ => {}
        }
    }
    if total_kb == 0 {
        return None;
    }
    Some(MemStatus {
        total_mb: total_kb / 1024,
        used_mb: total_kb.saturating_sub(avail_kb) / 1024,
    })
}

/// Ask llama-swap which models are currently running. Best effort: any failure
/// yields an empty list rather than an error.
pub fn query_running(listen: &str) -> Vec<String> {
    let addr = normalize_addr(listen);
    match http_get_json(&addr, "/running") {
        Some(v) => extract_running(&v),
        None => Vec::new(),
    }
}

fn normalize_addr(listen: &str) -> String {
    if let Some(port) = listen.strip_prefix(':') {
        format!("127.0.0.1:{port}")
    } else if listen.starts_with("0.0.0.0:") {
        listen.replacen("0.0.0.0", "127.0.0.1", 1)
    } else {
        listen.to_string()
    }
}

fn extract_running(v: &serde_json::Value) -> Vec<String> {
    let arr = v
        .get("running")
        .and_then(|r| r.as_array())
        .or_else(|| v.as_array());
    let Some(arr) = arr else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            item.get("model")
                .or_else(|| item.get("name"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// Minimal HTTP/1.0 GET returning the parsed JSON body (localhost only).
fn http_get_json(addr: &str, path: &str) -> Option<serde_json::Value> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(800)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(800)))
        .ok()?;
    let req = format!("GET {path} HTTP/1.0\r\nHost: {addr}\r\nAccept: application/json\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1)?;
    serde_json::from_str(body.trim()).ok()
}

/// Logged wrapper so callers can note sampling problems if desired.
#[allow(dead_code)]
pub fn log_sample_error(context: &str, msg: &str) {
    log(&format!("sample {context}: {msg}"));
}
