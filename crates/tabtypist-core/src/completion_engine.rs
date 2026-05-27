use crate::model_runtime::{truncate_at_sentence_boundary, Completer};
use anyhow::Result;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info};

#[cfg(not(test))]
const DEBOUNCE_MS: u64 = 150;
#[cfg(test)]
const DEBOUNCE_MS: u64 = 10;
const MAX_TOKENS: u32 = 25;

// ── Context ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionContext {
    pub prefix: String,
    pub suffix: String,
    pub caret_x: f64,
    pub caret_y: f64,
    pub caret_height: f64,
    pub app_bundle_id: String,
}

// ── Completion event ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompletionEvent {
    pub id: u64,
    pub text: String,
    pub context: CompletionContext,
}

// ── CancellationToken ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

// ── CompletionEngine ──────────────────────────────────────────────────────────

pub struct CompletionEngine {
    completer: Arc<dyn Completer>,
    event_tx: mpsc::Sender<CompletionEvent>,
    current_cancel: Mutex<Option<CancellationToken>>,
    next_id: AtomicU64,
}

impl CompletionEngine {
    pub fn new(
        completer: Arc<dyn Completer>,
    ) -> (Arc<Self>, mpsc::Receiver<CompletionEvent>) {
        let (tx, rx) = mpsc::channel(16);
        let engine = Arc::new(Self {
            completer,
            event_tx: tx,
            current_cancel: Mutex::new(None),
            next_id: AtomicU64::new(1),
        });
        (engine, rx)
    }

    /// Called for every context update from the sidecar.
    /// Cancels any in-flight completion, then debounces before generating a new one.
    pub async fn trigger(self: Arc<Self>, ctx: CompletionContext) {
        // Cancel previous in-flight request.
        {
            let mut guard = self.current_cancel.lock().await;
            if let Some(prev) = guard.take() {
                prev.cancel();
                debug!("cancelled in-flight completion");
            }
            let token = CancellationToken::new();
            *guard = Some(token.clone());
            // release lock before async work
            drop(guard);

            let engine = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS)).await;

                if token.is_cancelled() {
                    debug!("completion cancelled during debounce");
                    return;
                }

                let id = engine.next_id.fetch_add(1, Ordering::SeqCst);
                debug!("generating completion id={id} prefix={:?}", &ctx.prefix);

                let completer = engine.completer.clone();
                let prefix_clone = ctx.prefix.clone();
                let result = tokio::task::spawn_blocking(move || {
                    completer.complete(&prefix_clone, MAX_TOKENS)
                })
                .await;

                if token.is_cancelled() {
                    debug!("completion cancelled after inference");
                    return;
                }

                match result {
                    Ok(Ok(raw)) if !raw.is_empty() => {
                        let text = truncate_at_sentence_boundary(raw);
                        if text.is_empty() { return; }
                        let event = CompletionEvent { id, text, context: ctx };
                        info!("completion ready id={id}");
                        let _ = engine.event_tx.send(event).await;
                    }
                    Ok(Ok(_)) => debug!("empty completion, skipping"),
                    Ok(Err(e)) => tracing::warn!("completion error: {e}"),
                    Err(e) => tracing::warn!("completion task panic: {e}"),
                }
            });
        }
    }

    /// Called when the user types something that diverges from the current
    /// completion — dismiss without generating a new one.
    pub async fn dismiss_current(&self) {
        let mut guard = self.current_cancel.lock().await;
        if let Some(t) = guard.take() {
            t.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_runtime::StubCompleter;
    use std::time::Duration;

    fn ctx(prefix: &str) -> CompletionContext {
        CompletionContext {
            prefix: prefix.to_string(),
            suffix: String::new(),
            caret_x: 0.0,
            caret_y: 0.0,
            caret_height: 16.0,
            app_bundle_id: "com.apple.Notes".to_string(),
        }
    }

    // Poll rx until an event arrives or the deadline passes.
    async fn recv_timeout(rx: &mut mpsc::Receiver<CompletionEvent>, ms: u64) -> Option<CompletionEvent> {
        tokio::time::timeout(Duration::from_millis(ms), rx.recv()).await.ok().flatten()
    }

    #[tokio::test]
    async fn debounce_coalesces_burst() {
        let stub = Arc::new(StubCompleter {
            response: "world.".to_string(),
        });
        let (engine, mut rx) = CompletionEngine::new(stub);

        // Fire three triggers in rapid succession — only the last should produce a completion.
        engine.clone().trigger(ctx("Hello ")).await;
        engine.clone().trigger(ctx("Hello w")).await;
        engine.clone().trigger(ctx("Hello wo")).await;

        // Wait long enough for debounce to fire (DEBOUNCE_MS=10 in tests) + inference
        let event = recv_timeout(&mut rx, 500)
            .await
            .expect("expected a completion event after burst");
        assert_eq!(event.text, "world.");

        // No second event within a short window
        assert!(recv_timeout(&mut rx, 50).await.is_none(), "only one completion expected after burst");
    }

    #[tokio::test]
    async fn cancellation_token_cancels_inflight() {
        use std::sync::atomic::AtomicUsize;
        static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

        struct CountingCompleter;
        impl Completer for CountingCompleter {
            fn complete(&self, _: &str, _: u32) -> Result<String> {
                CALL_COUNT.fetch_add(1, Ordering::SeqCst);
                Ok("ok.".to_string())
            }
        }

        let (engine, mut rx) = CompletionEngine::new(Arc::new(CountingCompleter));
        engine.clone().trigger(ctx("first ")).await;
        // Cancel immediately before debounce expires
        engine.dismiss_current().await;

        // The cancelled trigger must not emit an event within the debounce+margin window
        assert!(recv_timeout(&mut rx, 200).await.is_none(), "cancelled completion must not emit");
    }

    #[tokio::test]
    async fn sentence_boundary_truncation() {
        let stub = Arc::new(StubCompleter {
            response: "Hello world. This is extra.".to_string(),
        });
        let (engine, mut rx) = CompletionEngine::new(stub);
        engine.clone().trigger(ctx("Say ")).await;

        let event = recv_timeout(&mut rx, 500)
            .await
            .expect("expected event");
        assert_eq!(event.text, "Hello world.", "must stop at first sentence boundary");
    }

    #[tokio::test]
    async fn token_cap_applied() {
        use std::sync::atomic::AtomicU32;
        static SAW_MAX: AtomicU32 = AtomicU32::new(0);

        struct MaxCapSpy;
        impl Completer for MaxCapSpy {
            fn complete(&self, _: &str, max_tokens: u32) -> Result<String> {
                SAW_MAX.store(max_tokens, Ordering::SeqCst);
                Ok(String::new())
            }
        }

        let (engine, _rx) = CompletionEngine::new(Arc::new(MaxCapSpy));
        engine.clone().trigger(ctx("test")).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(SAW_MAX.load(Ordering::SeqCst), MAX_TOKENS);
    }
}
