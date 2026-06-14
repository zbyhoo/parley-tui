use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use crate::router::AgentId;

/// Wynik moderacji zwracany handlerowi `send_to_peer`.
#[derive(Debug)]
pub enum Outcome {
    /// Zatwierdzone i wstrzyknięte do peera.
    Delivered,
    /// Odrzucone przez użytkownika.
    Rejected,
    /// Nie dostarczono z przyczyny technicznej (peer nie działa, shutdown).
    Error(String),
}

/// Oczekująca wiadomość agent→agent czekająca na moderację.
pub struct PendingMessage {
    pub from: AgentId,
    pub to: AgentId,
    pub text: String,
    /// Kanał zwrotny do zablokowanego handlera brokera.
    pub responder: oneshot::Sender<Outcome>,
}

/// Współdzielona kolejka FIFO — pomost wątek brokera (async) ↔ pętla UI (sync).
pub type PendingQueue = Arc<Mutex<VecDeque<PendingMessage>>>;

pub fn new_queue() -> PendingQueue {
    Arc::new(Mutex::new(VecDeque::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_order_and_resolve() {
        let q = new_queue();
        let (tx_a, mut rx_a) = oneshot::channel();
        let (tx_b, _rx_b) = oneshot::channel();
        q.lock().unwrap().push_back(PendingMessage {
            from: AgentId::Claude,
            to: AgentId::Codex,
            text: "a".into(),
            responder: tx_a,
        });
        q.lock().unwrap().push_back(PendingMessage {
            from: AgentId::Codex,
            to: AgentId::Claude,
            text: "b".into(),
            responder: tx_b,
        });
        // FIFO: pierwsza wyjmowana to "a"
        let first = q.lock().unwrap().pop_front().unwrap();
        assert_eq!(first.text, "a");
        assert_eq!(first.to, AgentId::Codex);
        first.responder.send(Outcome::Delivered).unwrap();
        assert!(matches!(rx_a.try_recv(), Ok(Outcome::Delivered)));
        // druga wciąż w kolejce
        assert_eq!(q.lock().unwrap().len(), 1);
    }
}
