use std::time::{Duration, Instant};

use parley_tui::broker;
use parley_tui::pending::{new_queue, Outcome};
use parley_tui::router::AgentId;

/// Wyciąga JSON z odpowiedzi streamable HTTP (SSE): bierze ostatnią linię `data: {...}`.
fn sse_json(body: &str) -> serde_json::Value {
    let line = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|s| s.trim_start().starts_with('{'))
        .last()
        .expect("brak linii data: z JSON w odpowiedzi SSE");
    serde_json::from_str(line).expect("niepoprawny JSON w SSE")
}

/// Pełny przepływ klienta MCP po HTTP: initialize → notifications/initialized → tools/call.
/// Zwraca tekst pierwszego Content z wyniku tools/call.
fn call_send_to_peer(port: u16, agent_id: &str, message: &str) -> String {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let client = reqwest::blocking::Client::new();
    let headers = |req: reqwest::blocking::RequestBuilder| {
        req.header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("X-Agent-Id", agent_id)
    };

    // initialize — zwraca nagłówek mcp-session-id
    let init = headers(client.post(&url))
        .body(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
        )
        .send()
        .expect("initialize failed");
    let session = init
        .headers()
        .get("mcp-session-id")
        .expect("brak mcp-session-id")
        .to_str()
        .unwrap()
        .to_string();
    let _ = init.text();

    // notifications/initialized
    headers(client.post(&url))
        .header("mcp-session-id", &session)
        .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .send()
        .expect("initialized failed");

    // tools/call send_to_peer — blokuje aż moderacja rozwiąże oneshot
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"send_to_peer","arguments":{{"message":"{message}"}}}}}}"#
    );
    let resp = headers(client.post(&url))
        .header("mcp-session-id", &session)
        .body(body)
        .send()
        .expect("tools/call failed");
    let json = sse_json(&resp.text().unwrap());
    json["result"]["content"][0]["text"]
        .as_str()
        .expect("brak tekstu w wyniku tools/call")
        .to_string()
}

#[test]
fn send_to_peer_lands_in_queue_and_resolves() {
    let queue = new_queue();
    let handle = broker::start(queue.clone()).unwrap();
    let port = handle.port;

    // Wątek drenujący: czeka aż wiadomość pojawi się w kolejce i ją zatwierdza.
    let drain_queue = queue.clone();
    let drainer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(msg) = drain_queue.lock().unwrap().pop_front() {
                assert_eq!(msg.from, AgentId::Claude);
                assert_eq!(msg.to, AgentId::Codex);
                assert_eq!(msg.text, "hello peer");
                msg.responder.send(Outcome::Delivered).unwrap();
                return;
            }
            assert!(Instant::now() < deadline, "wiadomość nie dotarła do kolejki");
            std::thread::sleep(Duration::from_millis(20));
        }
    });

    // Klient woła send_to_peer z X-Agent-Id: claude; oczekiwany wynik "delivered to codex".
    let result = call_send_to_peer(port, "claude", "hello peer");
    assert!(result.contains("delivered to codex"), "wynik: {result}");

    drainer.join().unwrap();
    handle.shutdown();
}

#[test]
fn unknown_agent_header_is_rejected() {
    let queue = new_queue();
    let handle = broker::start(queue.clone()).unwrap();
    // Nagłówek nieznany → handler zwraca błąd, nic nie ląduje w kolejce.
    let result = call_send_to_peer(handle.port, "nieznany", "x");
    assert!(result.contains("error"), "wynik: {result}");
    assert!(queue.lock().unwrap().is_empty());
    handle.shutdown();
}
