//! Rejestr live peerów headless: tożsamość, indeksy, kanały dostarczania, routing,
//! liveness (touch/reap) i recykling id.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Rodzaj wiadomości wstrzykiwanej do agenta przez wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsgKind {
    /// Wiadomość od innego peera — wrapper dokleja instrukcję odpowiedzi.
    Peer,
    /// Komunikat systemowy parley (np. anons dołączenia) — wstrzykiwany dosłownie.
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingMsg {
    pub from: String,
    pub text: String,
    pub kind: MsgKind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegisterError {
    Collision(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Delivered(String),
    Queued(String),
    NoSuchPeer { to: String, present: Vec<String> },
    NotRegistered,
}

struct Peer {
    binary: String,
    sender: Option<mpsc::Sender<IncomingMsg>>,
    last_seen: Instant,
}

pub struct Registry {
    peers: HashMap<String, Peer>,
}

impl Registry {
    pub fn new() -> Self {
        Registry { peers: HashMap::new() }
    }

    /// Najniższe wolne id dla danej binarki: `binary`, potem `binary-2`, `binary-3`…
    /// Recykluje zwolnione nazwy (w przeciwieństwie do monotonicznego licznika).
    fn alloc_id(&self, binary: &str) -> String {
        if !self.peers.contains_key(binary) {
            return binary.to_string();
        }
        let mut n = 2;
        loop {
            let cand = format!("{binary}-{n}");
            if !self.peers.contains_key(&cand) {
                return cand;
            }
            n += 1;
        }
    }

    pub fn register(
        &mut self,
        binary: &str,
        as_id: Option<&str>,
    ) -> Result<(String, mpsc::Receiver<IncomingMsg>), RegisterError> {
        let id = match as_id {
            Some(a) => {
                if self.peers.contains_key(a) {
                    return Err(RegisterError::Collision(a.to_string()));
                }
                a.to_string()
            }
            None => self.alloc_id(binary),
        };
        let (tx, rx) = mpsc::channel(256);
        self.peers.insert(
            id.clone(),
            Peer { binary: binary.to_string(), sender: Some(tx), last_seen: Instant::now() },
        );
        Ok((id, rx))
    }

    pub fn register_mcp_only(&mut self, id: &str) -> Result<(), RegisterError> {
        if self.peers.contains_key(id) {
            return Err(RegisterError::Collision(id.to_string()));
        }
        self.peers.insert(
            id.to_string(),
            Peer { binary: id.to_string(), sender: None, last_seen: Instant::now() },
        );
        Ok(())
    }

    pub fn deregister(&mut self, id: &str) {
        self.peers.remove(id);
    }

    pub fn is_live(&self, id: &str) -> bool {
        self.peers.contains_key(id)
    }

    /// Odświeża znacznik życia peera (wołane przy każdym /poll).
    pub fn touch(&mut self, id: &str, now: Instant) {
        if let Some(p) = self.peers.get_mut(id) {
            p.last_seen = now;
        }
    }

    /// Usuwa peerów, których ostatni kontakt jest starszy niż `ttl`. Zwraca usunięte id.
    pub fn reap(&mut self, now: Instant, ttl: Duration) -> Vec<String> {
        let dead: Vec<String> = self
            .peers
            .iter()
            .filter(|(_, p)| now.duration_since(p.last_seen) > ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &dead {
            self.peers.remove(id);
        }
        dead
    }

    pub fn list(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> =
            self.peers.iter().map(|(id, p)| (id.clone(), p.binary.clone())).collect();
        v.sort();
        v
    }

    /// Wysyła komunikat systemowy do wszystkich żywych peerów poza `except`.
    pub fn send_system_to_all(&self, text: &str, except: &str) {
        for (id, p) in &self.peers {
            if id == except {
                continue;
            }
            if let Some(tx) = &p.sender {
                let _ = tx.try_send(IncomingMsg {
                    from: "parley".to_string(),
                    text: text.to_string(),
                    kind: MsgKind::System,
                });
            }
        }
    }

    pub fn route(&mut self, from: &str, to: &str, text: &str) -> Vec<SendOutcome> {
        if !self.peers.contains_key(from) {
            return vec![SendOutcome::NotRegistered];
        }
        let targets: Vec<String> = if to == "all" {
            self.peers.keys().filter(|k| *k != from).cloned().collect()
        } else {
            vec![to.to_string()]
        };
        let mut out = Vec::new();
        for t in targets {
            match self.peers.get(&t).and_then(|p| p.sender.clone()) {
                Some(tx) => {
                    let msg = IncomingMsg {
                        from: from.to_string(),
                        text: text.to_string(),
                        kind: MsgKind::Peer,
                    };
                    match tx.try_send(msg) {
                        Ok(()) => out.push(SendOutcome::Delivered(t)),
                        Err(_) => out.push(SendOutcome::Queued(t)),
                    }
                }
                None if self.peers.contains_key(&t) => {
                    out.push(SendOutcome::Queued(t));
                }
                None => {
                    let mut present = self.peers.keys().cloned().collect::<Vec<_>>();
                    present.sort();
                    out.push(SendOutcome::NoSuchPeer { to: t, present });
                }
            }
        }
        out
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_ids_recycle_lowest_free() {
        let mut r = Registry::new();
        let (id1, _rx1) = r.register("claude", None).unwrap();
        let (id2, _rx2) = r.register("claude", None).unwrap();
        assert_eq!(id1, "claude");
        assert_eq!(id2, "claude-2");
        r.deregister("claude"); // zwolnij najniższy
        let (id3, _rx3) = r.register("claude", None).unwrap();
        assert_eq!(id3, "claude", "wolne id jest recyklowane");
    }

    #[test]
    fn auto_id_skips_live_taken_name() {
        let mut r = Registry::new();
        let (_id, _rx) = r.register("x", Some("codex")).unwrap(); // żywy peer "codex"
        let (auto, _rx2) = r.register("codex", None).unwrap();
        assert_ne!(auto, "codex", "auto-id nie nadpisuje żywego peera");
        assert_eq!(auto, "codex-2");
        assert!(r.is_live("codex"));
    }

    #[test]
    fn as_override_collision_is_error() {
        let mut r = Registry::new();
        let (_id, _rx) = r.register("claude", Some("reviewer")).unwrap();
        assert!(matches!(
            r.register("codex", Some("reviewer")),
            Err(RegisterError::Collision(ref s)) if s == "reviewer"
        ));
    }

    #[test]
    fn route_to_unknown_lists_present() {
        let mut r = Registry::new();
        let (from, _rx) = r.register("claude", None).unwrap();
        let out = r.route(&from, "codex", "hi");
        assert_eq!(
            out,
            vec![SendOutcome::NoSuchPeer { to: "codex".into(), present: vec!["claude".into()] }]
        );
    }

    #[test]
    fn route_from_unregistered_is_not_registered() {
        let mut r = Registry::new();
        assert_eq!(r.route("ghost", "claude", "hi"), vec![SendOutcome::NotRegistered]);
    }

    #[tokio::test]
    async fn route_delivers_peer_kind_to_channel() {
        let mut r = Registry::new();
        let (claude, _rxc) = r.register("claude", None).unwrap();
        let (_codex, mut rxx) = r.register("codex", None).unwrap();
        let out = r.route(&claude, "codex", "ping");
        assert_eq!(out, vec![SendOutcome::Delivered("codex".into())]);
        let msg = rxx.recv().await.unwrap();
        assert_eq!(
            msg,
            IncomingMsg { from: "claude".into(), text: "ping".into(), kind: MsgKind::Peer }
        );
    }

    #[tokio::test]
    async fn broadcast_all_skips_sender() {
        let mut r = Registry::new();
        let (claude, _rxc) = r.register("claude", None).unwrap();
        let (_codex, mut rxx) = r.register("codex", None).unwrap();
        let (_gem, mut rxg) = r.register("gemini", None).unwrap();
        let out = r.route(&claude, "all", "hello");
        assert_eq!(out.len(), 2);
        assert_eq!(rxx.recv().await.unwrap().text, "hello");
        assert_eq!(rxg.recv().await.unwrap().text, "hello");
    }

    #[test]
    fn mcp_only_can_send_not_receive() {
        let mut r = Registry::new();
        r.register_mcp_only("claude").unwrap();
        assert!(r.is_live("claude"));
        let out = r.route("claude", "all", "x");
        assert_eq!(out, vec![]);
    }

    #[test]
    fn mcp_only_collision_with_live_peer() {
        let mut r = Registry::new();
        let (_id, _rx) = r.register("claude", None).unwrap();
        assert_eq!(r.register_mcp_only("claude"), Err(RegisterError::Collision("claude".into())));
    }

    #[test]
    fn reap_removes_stale_keeps_touched() {
        let mut r = Registry::new();
        let (a, _rxa) = r.register("claude", None).unwrap();
        let (_b, _rxb) = r.register("codex", None).unwrap();
        let t0 = Instant::now();
        let ttl = Duration::from_secs(60);
        // odśwież claude "w przyszłości", codex zostaw nieświeży
        r.touch(&a, t0 + Duration::from_secs(100));
        let reaped = r.reap(t0 + Duration::from_secs(120), ttl);
        // codex: last_seen≈t0, 120s > 60s → reaped; claude: last_seen=t0+100, 20s < 60s → żyje
        assert_eq!(reaped, vec!["codex".to_string()]);
        assert!(r.is_live("claude"));
        assert!(!r.is_live("codex"));
    }

    #[test]
    fn reap_after_empty_allows_id_reset() {
        let mut r = Registry::new();
        let (_a, _rxa) = r.register("claude", None).unwrap();
        let t0 = Instant::now();
        let reaped = r.reap(t0 + Duration::from_secs(120), Duration::from_secs(60));
        assert_eq!(reaped, vec!["claude".to_string()]);
        // registry puste → następny claude znów "claude"
        let (id, _rx) = r.register("claude", None).unwrap();
        assert_eq!(id, "claude");
    }

    #[tokio::test]
    async fn system_broadcast_reaches_others_not_sender() {
        let mut r = Registry::new();
        let (_a, mut rxa) = r.register("claude", None).unwrap();
        let (b, mut rxb) = r.register("codex", None).unwrap();
        r.send_system_to_all("hello-system", &b); // except = codex (the joiner)
        // claude (nie-except) dostaje System
        let msg = rxa.recv().await.unwrap();
        assert_eq!(msg.kind, MsgKind::System);
        assert_eq!(msg.from, "parley");
        assert_eq!(msg.text, "hello-system");
        // codex (except) nic nie dostaje
        assert!(rxb.try_recv().is_err());
    }
}
