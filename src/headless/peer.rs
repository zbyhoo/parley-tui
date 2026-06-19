//! Rejestr live peerów headless: tożsamość, indeksy, kanały dostarczania, routing.

use std::collections::HashMap;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IncomingMsg {
    pub from: String,
    pub text: String,
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
}

pub struct Registry {
    peers: HashMap<String, Peer>,
    next_idx: HashMap<String, u32>,
}

impl Registry {
    pub fn new() -> Self {
        Registry { peers: HashMap::new(), next_idx: HashMap::new() }
    }

    fn alloc_id(&mut self, binary: &str) -> String {
        let n = self.next_idx.entry(binary.to_string()).or_insert(1);
        loop {
            let id = if *n == 1 { binary.to_string() } else { format!("{binary}-{n}") };
            *n += 1;
            if !self.peers.contains_key(&id) {
                return id;
            }
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
        self.peers.insert(id.clone(), Peer { binary: binary.to_string(), sender: Some(tx) });
        Ok((id, rx))
    }

    pub fn register_mcp_only(&mut self, id: &str) -> Result<(), RegisterError> {
        if self.peers.contains_key(id) {
            return Err(RegisterError::Collision(id.to_string()));
        }
        // binarka = id (najlepsze przybliżenie dla gołego CLI)
        self.peers.insert(id.to_string(), Peer { binary: id.to_string(), sender: None });
        Ok(())
    }

    pub fn deregister(&mut self, id: &str) {
        self.peers.remove(id);
    }

    pub fn is_live(&self, id: &str) -> bool {
        self.peers.contains_key(id)
    }

    pub fn list(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> =
            self.peers.iter().map(|(id, p)| (id.clone(), p.binary.clone())).collect();
        v.sort();
        v
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
                    let msg = IncomingMsg { from: from.to_string(), text: text.to_string() };
                    match tx.try_send(msg) {
                        Ok(()) => out.push(SendOutcome::Delivered(t)),
                        Err(_) => out.push(SendOutcome::Queued(t)), // kanał pełny / bez odbiorcy
                    }
                }
                None if self.peers.contains_key(&t) => {
                    // peer MCP-only (bez kanału) — nie da się dostarczyć push
                    out.push(SendOutcome::Queued(t));
                }
                None => {
                    let present = self.peers.keys().cloned().collect::<Vec<_>>();
                    let mut present = present;
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
    fn auto_ids_are_monotonic_no_reuse() {
        let mut r = Registry::new();
        let (id1, _rx1) = r.register("claude", None).unwrap();
        let (id2, _rx2) = r.register("claude", None).unwrap();
        assert_eq!(id1, "claude");
        assert_eq!(id2, "claude-2");
        r.deregister("claude"); // zwolnij pierwszego
        let (id3, _rx3) = r.register("claude", None).unwrap();
        assert_eq!(id3, "claude-3", "indeks nie jest recyklowany");
    }

    #[test]
    fn as_override_collision_is_error() {
        let mut r = Registry::new();
        let (_id, _rx) = r.register("claude", Some("reviewer")).unwrap();
        let result = r.register("codex", Some("reviewer"));
        assert!(matches!(result, Err(RegisterError::Collision(ref s)) if s == "reviewer"));
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
    async fn route_delivers_to_channel() {
        let mut r = Registry::new();
        let (claude, _rxc) = r.register("claude", None).unwrap();
        let (_codex, mut rxx) = r.register("codex", None).unwrap();
        let out = r.route(&claude, "codex", "ping");
        assert_eq!(out, vec![SendOutcome::Delivered("codex".into())]);
        let msg = rxx.recv().await.unwrap();
        assert_eq!(msg, IncomingMsg { from: "claude".into(), text: "ping".into() });
    }

    #[tokio::test]
    async fn broadcast_all_skips_sender() {
        let mut r = Registry::new();
        let (claude, _rxc) = r.register("claude", None).unwrap();
        let (_codex, mut rxx) = r.register("codex", None).unwrap();
        let (_gem, mut rxg) = r.register("gemini", None).unwrap();
        let mut out = r.route(&claude, "all", "hello");
        out.sort_by_key(|o| format!("{o:?}"));
        assert_eq!(out.len(), 2);
        assert_eq!(rxx.recv().await.unwrap().text, "hello");
        assert_eq!(rxg.recv().await.unwrap().text, "hello");
    }

    #[test]
    fn mcp_only_can_send_not_receive() {
        let mut r = Registry::new();
        r.register_mcp_only("claude").unwrap();
        assert!(r.is_live("claude"));
        let out = r.route("claude", "all", "x"); // brak innych peerów
        assert_eq!(out, vec![]); // nikogo do dostarczenia, ale from był live → nie NotRegistered
    }

    #[test]
    fn mcp_only_collision_with_live_peer() {
        let mut r = Registry::new();
        let (_id, _rx) = r.register("claude", None).unwrap();
        assert_eq!(r.register_mcp_only("claude"), Err(RegisterError::Collision("claude".into())));
    }

    #[test]
    fn auto_id_skips_manually_taken_name() {
        let mut r = Registry::new();
        let (_id, _rx) = r.register("x", Some("codex")).unwrap(); // manual peer named "codex"
        let (auto, _rx2) = r.register("codex", None).unwrap();      // auto for binary "codex"
        assert_ne!(auto, "codex", "auto id must not overwrite the live manual peer");
        assert!(r.is_live("codex"));
    }
}
