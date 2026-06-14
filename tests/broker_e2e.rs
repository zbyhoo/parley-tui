//! End-to-end bez prawdziwych CLI: broker (HTTP) → kolejka → App.moderate (Alt+A)
//! → wstrzyknięcie do PTY peera (fake sh). Pokrywa szew wiringu, którego nie
//! dotykają testy jednostkowe broker/app osobno.

use std::time::{Duration, Instant};

use parley_tui::app::App;
use parley_tui::broker;
use parley_tui::config::{AgentConfig, Config};
use parley_tui::pending::new_queue;
use parley_tui::router::AgentId;
use parley_tui::timeline::Timeline;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn sse_json(body: &str) -> serde_json::Value {
    let line = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|s| s.trim_start().starts_with('{'))
        .last()
        .expect("brak data: JSON w SSE");
    serde_json::from_str(line).unwrap()
}

fn call_send_to_peer(port: u16, agent_id: &str, message: &str) -> String {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let client = reqwest::blocking::Client::new();
    let hdr = |r: reqwest::blocking::RequestBuilder| {
        r.header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("X-Agent-Id", agent_id)
    };
    let init = hdr(client.post(&url))
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#)
        .send()
        .unwrap();
    let session =
        init.headers().get("mcp-session-id").unwrap().to_str().unwrap().to_string();
    let _ = init.text();
    hdr(client.post(&url))
        .header("mcp-session-id", &session)
        .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .send()
        .unwrap();
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"send_to_peer","arguments":{{"message":"{message}"}}}}}}"#
    );
    let resp =
        hdr(client.post(&url)).header("mcp-session-id", &session).body(body).send().unwrap();
    let json = sse_json(&resp.text().unwrap());
    json["result"]["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn approved_peer_message_is_injected_into_peer_pty() {
    let dir = tempfile::tempdir().unwrap();
    let tl = Timeline::open(&dir.path().join("timeline.jsonl")).unwrap();
    let cwd = dir.path().to_path_buf();
    let pending = new_queue();
    let mut app = App::new(Config::default(), tl, cwd, pending.clone());

    // Peer (codex) = fake agent echujący wejście, żeby wstrzyknięcie było widoczne na ekranie.
    app.pane_mut(AgentId::Codex).cfg = AgentConfig {
        command: "sh".into(),
        args: vec![
            "-c".into(),
            "while IFS= read -r l; do echo \"GOT:$l\"; done".into(),
        ],
        resume_command: None,
    };
    app.spawn_agent(AgentId::Codex);

    let handle = broker::start(pending.clone()).unwrap();
    let port = handle.port;

    // Klient (claude) woła send_to_peer w osobnym wątku — blokuje do moderacji.
    let client = std::thread::spawn(move || call_send_to_peer(port, "claude", "ping peer"));

    // App czeka aż broker wrzuci wiadomość, po czym zatwierdza (Alt+A).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if !app.pending.lock().unwrap().is_empty() {
            app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
            break;
        }
        assert!(Instant::now() < deadline, "broker nie wrzucił wiadomości do kolejki");
        std::thread::sleep(Duration::from_millis(20));
    }

    // Klient powinien dostać potwierdzenie dostarczenia.
    let result = client.join().unwrap();
    assert!(result.contains("delivered to codex"), "wynik: {result}");

    // Peer PTY powinno zobaczyć wstrzyknięty prompt z prefiksem.
    let codex = app.pane(AgentId::Codex);
    let proc = codex.proc.as_ref().unwrap();
    let mut seen = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let screen = proc.with_screen(|s| s.contents());
        if screen.contains("incoming message") && screen.contains("ping peer") {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(seen, "peer PTY nie pokazało wstrzykniętej wiadomości");

    handle.shutdown();
    app.shutdown();
}
