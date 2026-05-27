use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use tracing::{debug, info, warn};

const TELEMETRY_ENDPOINT: &str = "https://telemetry.tabtypist.com/v1/events";

// ── Event types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TelemetryEvent {
    AppLaunched {
        app_version: String,
        os_version: String,
        model_id: String,
    },
    CompletionAccepted {
        model_id: String,
    },
    CompletionDismissed {
        model_id: String,
    },
    CrashReport {
        app_version: String,
        stack_trace: String,
    },
}

#[derive(Debug, Clone, Serialize)]
struct Batch {
    install_id: String,
    events: Vec<TelemetryEvent>,
}

// ── TelemetryClient ───────────────────────────────────────────────────────────

pub struct TelemetryClient {
    install_id: String,
    /// True only after the user has explicitly consented.
    consent: Arc<Mutex<bool>>,
    queue: Arc<Mutex<Vec<TelemetryEvent>>>,
}

impl TelemetryClient {
    pub fn new(install_id: impl Into<String>, consent: bool) -> Self {
        Self {
            install_id: install_id.into(),
            consent: Arc::new(Mutex::new(consent)),
            queue: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Update consent.  If revoked, the in-memory queue is cleared.
    pub fn set_consent(&self, enabled: bool) {
        *self.consent.lock().unwrap() = enabled;
        if !enabled {
            self.queue.lock().unwrap().clear();
            debug!("telemetry consent revoked; queue cleared");
        }
    }

    /// Enqueue an event.  No-op if consent is false.
    pub fn record(&self, event: TelemetryEvent) {
        if !*self.consent.lock().unwrap() {
            return; // never enqueue before consent
        }
        self.queue.lock().unwrap().push(event);
    }

    /// Flush the queue to the endpoint.  No-op if consent is false.
    pub async fn flush(&self) -> Result<()> {
        if !*self.consent.lock().unwrap() {
            return Ok(());
        }

        let events: Vec<TelemetryEvent> = {
            let mut q = self.queue.lock().unwrap();
            std::mem::take(&mut *q)
        };

        if events.is_empty() {
            return Ok(());
        }

        let batch = Batch {
            install_id: self.install_id.clone(),
            events,
        };

        let client = reqwest::Client::new();
        let resp = client
            .post(TELEMETRY_ENDPOINT)
            .json(&batch)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                info!("telemetry flushed {} events", batch.events.len());
            }
            Ok(r) => warn!("telemetry flush failed: HTTP {}", r.status()),
            Err(e) => warn!("telemetry flush network error: {e}"),
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_events_before_consent() {
        let client = TelemetryClient::new("test-id", false);
        client.record(TelemetryEvent::CompletionAccepted {
            model_id: "qwen2.5-1.5b-q4".to_string(),
        });
        let q = client.queue.lock().unwrap();
        assert!(q.is_empty(), "must not enqueue events before consent");
    }

    #[test]
    fn events_recorded_after_consent() {
        let client = TelemetryClient::new("test-id", false);
        client.set_consent(true);
        client.record(TelemetryEvent::CompletionAccepted {
            model_id: "qwen2.5-1.5b-q4".to_string(),
        });
        let q = client.queue.lock().unwrap();
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn revoke_consent_clears_queue() {
        let client = TelemetryClient::new("test-id", true);
        client.record(TelemetryEvent::CompletionAccepted {
            model_id: "qwen2.5-1.5b-q4".to_string(),
        });
        client.set_consent(false);
        let q = client.queue.lock().unwrap();
        assert!(q.is_empty(), "revoking consent must clear the queue");
    }

    #[test]
    fn no_pii_in_completion_event() {
        // Verify that CompletionAccepted only contains model_id, not any user text.
        let event = TelemetryEvent::CompletionAccepted {
            model_id: "qwen2.5-1.5b-q4".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        // Must not contain field text (prefix/suffix) or actual completion content
        assert!(!json.contains("prefix"), "event must not contain field text (prefix)");
        assert!(!json.contains("\"text\""), "event must not contain raw completion text");
        // model_id is the only non-metadata field — verify it's there
        assert!(json.contains("qwen2.5-1.5b-q4"), "model_id must be recorded");
    }
}
