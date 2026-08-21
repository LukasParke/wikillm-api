//! Fan-out of change events to SSE/WebSocket clients.

use crate::domain::ChangeEvent;
use tokio::sync::{mpsc, RwLock};

#[derive(Clone)]
pub enum ClientSink {
    /// Server-sent events: pre-formatted payload
    Sse(mpsc::UnboundedSender<String>),
    /// WebSocket: raw JSON
    Ws(mpsc::UnboundedSender<String>),
}

#[derive(Default)]
pub struct Broadcaster {
    clients: RwLock<Vec<ClientSink>>,
}

impl Broadcaster {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn add(&self, sink: ClientSink) {
        self.clients.write().await.push(sink);
    }

    pub async fn remove_sse(&self, tx: &mpsc::UnboundedSender<String>) {
        self.clients
            .write()
            .await
            .retain(|c| !matches!(c, ClientSink::Sse(t) if t.same_channel(tx)));
    }

    pub async fn remove_ws(&self, tx: &mpsc::UnboundedSender<String>) {
        self.clients
            .write()
            .await
            .retain(|c| !matches!(c, ClientSink::Ws(t) if t.same_channel(tx)));
    }

    pub async fn broadcast(&self, event: &ChangeEvent) {
        let payload = serde_json::to_string(event).unwrap_or_default();
        let sse = format!("event: change\ndata: {payload}\n\n");
        let clients = self.clients.read().await;
        for client in clients.iter() {
            match client {
                ClientSink::Sse(tx) => {
                    let _ = tx.send(sse.clone());
                }
                ClientSink::Ws(tx) => {
                    let _ = tx.send(payload.clone());
                }
            }
        }
    }

    pub async fn counts(&self) -> (usize, usize) {
        let clients = self.clients.read().await;
        (
            clients.iter().filter(|c| matches!(c, ClientSink::Sse(_))).count(),
            clients.iter().filter(|c| matches!(c, ClientSink::Ws(_))).count(),
        )
    }
}
