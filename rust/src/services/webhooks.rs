//! Outbound webhook delivery with HMAC signing and retries.

use crate::store::Store;
use crate::domain::WebhookRecord;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;

const RETRY_DELAYS_MS: &[u64] = &[250, 1000, 4000];

pub struct WebhookDispatcher {
    store: Arc<dyn Store>,
    settings: Arc<crate::services::settings::SettingsService>,
}

fn match_prefixes(prefixes: &[String], rel_path: &str) -> bool {
    for raw in prefixes {
        let prefix = raw.strip_suffix('/').unwrap_or(raw);
        if prefix == "*" {
            return true;
        }
        if rel_path == prefix || rel_path.starts_with(&format!("{prefix}/")) {
            return true;
        }
    }
    false
}

impl WebhookDispatcher {
    pub fn new(
        store: Arc<dyn Store>,
        settings: Arc<crate::services::settings::SettingsService>,
    ) -> Self {
        Self { store, settings }
    }

    /// Fire-and-forget delivery to every enabled matching subscriber.
    pub async fn dispatch(&self, event: &crate::domain::ChangeEventData) {
        let hooks = match self.store.list_webhooks().await {
            Ok(h) => h,
            Err(_) => return,
        };
        let matching: Vec<&WebhookRecord> = hooks
            .iter()
            .filter(|h| h.enabled && h.events.iter().any(|e| e == "change"))
            .filter(|h| match_prefixes(&h.prefixes, &event.rel_path))
            .collect();
        for hook in matching {
            let _ = self.deliver(hook.clone(), event).await;
        }
    }

    async fn deliver(&self, hook: WebhookRecord, event: &crate::domain::ChangeEventData) {
        let client = reqwest::Client::new();
        let body = serde_json::to_string(event).unwrap_or_default();
        let secret = self.settings.get_string("webhook_secret").await.unwrap_or_default();
        let mut last_status = "unknown".to_string();
        for attempt in 0..=RETRY_DELAYS_MS.len() {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAYS_MS[attempt - 1]))
                    .await;
            }
            let mut request = client
                .post(&hook.url)
                .header("Content-Type", "application/json")
                .header("X-WikiLLM-Event", "change")
                .timeout(std::time::Duration::from_secs(10));
            if !secret.is_empty() {
                let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                    .expect("hmac accepts any key length");
                mac.update(body.as_bytes());
                request = request.header(
                    "X-WikiLLM-Signature",
                    format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
                );
            }
            match request.body(body.clone()).send().await {
                Ok(response) => {
                    last_status = response.status().as_u16().to_string();
                    if response.status().is_success() {
                        break;
                    }
                }
                Err(e) => last_status = format!("error: {:.120}", e),
            }
        }
        let _ = self.store.record_webhook_attempt(&hook.id, &last_status).await;
    }
}
