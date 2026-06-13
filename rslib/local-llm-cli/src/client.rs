//! Thin client for the resourced control socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result};
use local_llm_config::runtime::{Request, Response};
use local_llm_config::Runtime;

/// Send a single request to resourced and return its response.
pub fn request(rt: &Runtime, req: &Request) -> Result<Response> {
    let path = rt.socket_path();
    let stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "connecting to resourced at {} (is local-llm-resourced.service running?)",
            path.display()
        )
    })?;

    let mut writer = stream.try_clone()?;
    let mut payload = serde_json::to_string(req)?;
    payload.push('\n');
    writer.write_all(payload.as_bytes())?;
    writer.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        anyhow::bail!("empty response from resourced");
    }
    let resp: Response = serde_json::from_str(line.trim())?;
    Ok(resp)
}
