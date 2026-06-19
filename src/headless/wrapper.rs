//! Orkiestracja wrappera headless: register → spawn PTY proxy → long-poll → deregister.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::headless::discovery::{ensure_broker, BrokerInfo};
use crate::headless::proxy::{Proxy, ProxyHandle};

pub fn run(as_id: Option<String>, command: Vec<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let state_dir = cwd.join(".parley");
    let self_exe = std::env::current_exe()?;
    let info = ensure_broker(&state_dir, &cwd, &self_exe)?;

    let binary = command.first().context("empty command")?.clone();
    let (id, peers) = register(&info, &binary, as_id.as_deref())?;
    eprintln!("[parley] connected as '{id}' (broker port {})", info.port);

    // MCP args injection
    let cfg_path = state_dir.join(format!("mcp-{id}.json"));
    let base = Path::new(&binary).file_name().and_then(|s| s.to_str()).unwrap_or(&binary);
    if !base.starts_with("codex") {
        crate::config::write_mcp_config_json(&cfg_path, &id, info.port, &info.token)
            .map_err(|e| anyhow::anyhow!("write_mcp_config_json: {e}"))?;
    }
    let mut full = command.clone();
    let extra = crate::config::mcp_args_for(&binary, &id, info.port, &info.token, &cfg_path);
    full.extend(extra);

    let (handle, proxy_child) = Proxy::spawn(&full, &cwd)?;

    if !peers.is_empty() {
        let list = peers.join(", ");
        handle.inject(&format!(
            "[parley: connected peers: {list} — reach them with send_to_peer(to=\"<id>\")]"
        ));
    }

    // Long-poll thread gets a clone of the handle (no shared ownership of child).
    let stop = Arc::new(AtomicBool::new(false));
    let _poll_handle = {
        let info = info.clone();
        let id = id.clone();
        let poll_handle_clone = handle.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || poll_loop(&info, &id, &poll_handle_clone, &stop))
    };

    // Main thread owns the child; blocks here until agent exits.
    let code = proxy_child.wait();

    // Set stop flag so the poll thread exits its loop, then deregister.
    // We do NOT join the poll thread: it may be mid-long-poll (up to 30 s).
    // process::exit tears down the abandoned thread immediately, so we exit promptly.
    stop.store(true, Ordering::SeqCst);
    let _ = deregister(&info, &id);
    std::process::exit(code);
}

fn register(info: &BrokerInfo, binary: &str, as_id: Option<&str>) -> Result<(String, Vec<String>)> {
    let url = format!("http://127.0.0.1:{}/register?token={}", info.port, info.token);
    let body = serde_json::json!({ "binary": binary, "as": as_id });
    let resp: serde_json::Value = reqwest::blocking::Client::new()
        .post(&url).json(&body).send()?.json()?;
    if resp.get("error").and_then(|v| v.as_str()) == Some("collision") {
        anyhow::bail!("id '{}' already in use", as_id.unwrap_or(binary));
    }
    if let Some(id) = resp.get("id").and_then(|v| v.as_str()) {
        let peers = resp
            .get("peers")
            .and_then(|p| p.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>())
            .unwrap_or_default();
        return Ok((id.to_string(), peers));
    }
    anyhow::bail!("register failed: {resp}")
}

fn deregister(info: &BrokerInfo, id: &str) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/deregister?token={}", info.port, info.token);
    reqwest::blocking::Client::new()
        .post(&url)
        .json(&serde_json::json!({ "id": id }))
        .send()?;
    Ok(())
}

fn poll_loop(info: &BrokerInfo, id: &str, handle: &ProxyHandle, stop: &AtomicBool) {
    let url = format!("http://127.0.0.1:{}/poll/{id}?token={}", info.port, info.token);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    while !stop.load(Ordering::SeqCst) {
        match client.get(&url).send().and_then(|r| r.json::<serde_json::Value>()) {
            Ok(v) => {
                if let Some(msg) = v.get("message").filter(|m| !m.is_null()) {
                    let from = msg.get("from").and_then(|s| s.as_str()).unwrap_or("peer");
                    let text = msg.get("text").and_then(|s| s.as_str()).unwrap_or("");
                    let kind = msg.get("kind").and_then(|s| s.as_str()).unwrap_or("peer");
                    let injected = if kind == "system" {
                        text.to_string()
                    } else {
                        format!(
                            "[incoming message from {from} — to reply, call the send_to_peer tool with to=\"{from}\"]: {text}"
                        )
                    };
                    handle.inject(&injected);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}
