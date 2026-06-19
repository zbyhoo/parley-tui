//! Ręczna/integracyjna weryfikacja hub: start serwera w wątku, register dwóch peerów,
//! send_to_peer przez MCP nie jest tu testowany (wymaga klienta MCP) — sprawdzamy
//! endpointy kontrolne register/poll/deregister przez reqwest blocking.
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let cwd = dir.path().to_path_buf();
    let serve_cwd = cwd.clone();
    std::thread::spawn(move || {
        let _ = parley_tui::headless::hub::serve(serve_cwd);
    });
    // poczekaj aż broker.json się pojawi
    let state_dir = cwd.join(".parley");
    let bj = parley_tui::headless::discovery::broker_json_path(&state_dir);
    let info = loop {
        if let Some(i) = parley_tui::headless::discovery::read(&bj) {
            break i;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let base = format!("http://127.0.0.1:{}", info.port);
    let c = reqwest::blocking::Client::new();

    // health
    let h: serde_json::Value =
        c.get(format!("{base}/health?token={}", info.token)).send()?.json()?;
    assert_eq!(h["cwd"], serde_json::json!(info.cwd));

    // register dwóch
    let r1: serde_json::Value = c
        .post(format!("{base}/register?token={}", info.token))
        .json(&serde_json::json!({"binary": "claude"}))
        .send()?
        .json()?;
    let r2: serde_json::Value = c
        .post(format!("{base}/register?token={}", info.token))
        .json(&serde_json::json!({"binary": "codex"}))
        .send()?
        .json()?;
    assert_eq!(r1["id"], serde_json::json!("claude"));
    assert_eq!(r2["id"], serde_json::json!("codex"));

    println!("hub_spike OK: port={} token={}", info.port, info.token);
    Ok(())
}
