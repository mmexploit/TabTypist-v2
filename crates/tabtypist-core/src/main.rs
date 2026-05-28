mod completion_engine;
mod exclusion_engine;
mod ipc;
mod language_router;
mod model_downloader;
mod model_runtime;
mod settings_store;
mod telemetry;

use anyhow::{Context, Result};
use completion_engine::CompletionContext;
use exclusion_engine::ExclusionEngine;
use ipc::{IpcTransport, RpcMessage};
use language_router::LanguageRouter;
use model_downloader::{ModelCatalog, ModelDownloader};
use settings_store::SettingsStore;
use std::sync::Arc;
use telemetry::{TelemetryClient, TelemetryEvent};
use tokio::sync::Mutex;
use tracing::{info, warn};

// The Rust core runs as a subprocess of the Swift app.
// Swift connects our stdin/stdout as a bidirectional JSON-RPC pipe.

/// Payload broadcast through the debounce channel on every contextUpdate.
#[derive(Clone, Debug)]
struct ContextUpdate {
    prefix:        String,
    suffix:        String,
    caret_x:       f64,
    caret_y:       f64,
    caret_height:  f64,
    app_bundle_id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr) // never pollute stdout (the IPC channel)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("tabtypist_core=info".parse().unwrap()),
        )
        .init();

    info!("TabTypist core starting (stdin→stdout IPC mode)");

    let settings = SettingsStore::load()?;

    // IPC: read from stdin, write to stdout.
    let transport = Arc::new(Mutex::new(IpcTransport::from_stdout()));
    let mut incoming = ipc::spawn_reader(tokio::io::stdin());

    // Handshake: wait for ping from Swift, reply pong.
    match incoming.recv().await {
        Some(msg) if msg.method.as_deref() == Some("ping") => {
            let id = msg.id.unwrap_or(0);
            transport.lock().await.respond(id, serde_json::json!("pong")).await?;
            info!("IPC handshake OK (ping id={id})");
        }
        other => {
            warn!("expected ping, got: {other:?}");
        }
    }

    // Load the English model if already installed.
    let models_dir = model_downloader::models_dir()?;
    let ed25519_pubkey = include_bytes!("../../../Resources/ed25519_pubkey.bin");
    let downloader = Arc::new(ModelDownloader::new(models_dir, *ed25519_pubkey));

    let router: Arc<Mutex<LanguageRouter>> = Arc::new(Mutex::new(LanguageRouter::new()));

    let en_entry = ModelCatalog::default_for_language("en")
        .context("no English model in catalog")?;
    if downloader.is_installed(&en_entry) {
        info!("loading English model from {:?}", downloader.installed_path(&en_entry));
        match model_runtime::LlamaCppCompleter::load(&downloader.installed_path(&en_entry)) {
            Ok(c) => {
                router.lock().await.register("en", Arc::new(c));
                info!("English model loaded");
            }
            Err(e) => warn!("failed to load English model: {e}"),
        }
    } else {
        info!("English model not installed — waiting for download");
    }

    let exclusion_engine = ExclusionEngine::with_built_in();
    let telemetry = TelemetryClient::new(
        settings.get().install_id.clone(),
        settings.get().telemetry_enabled,
    );

    let current_completion: Arc<Mutex<Option<completion_engine::CompletionEvent>>> =
        Arc::new(Mutex::new(None));

    // Single watch channel for context updates.
    // The debounce task reads from here; handle_message writes to it.
    // watch is a single-slot: only the LATEST value is kept, which is exactly
    // what we want — intermediate keystrokes while inference is running are
    // collapsed into one re-trigger after the cool-down.
    let (ctx_tx, ctx_rx) = tokio::sync::watch::channel::<Option<ContextUpdate>>(None);
    let ctx_tx = Arc::new(ctx_tx);

    // Spawn the single debounce + inference loop.
    // This is the ONLY place that ever calls completer.complete(), so at most
    // one LlamaContext exists at any moment.
    {
        let mut rx        = ctx_rx;
        let transport_inf = transport.clone();
        let current_inf   = current_completion.clone();
        let router_inf    = router.clone();
        let settings_inf  = settings.clone();

        tokio::spawn(async move {
            'outer: loop {
                // Block until a new context update arrives.
                if rx.changed().await.is_err() { break; }

                // Debounce: restart the 400 ms timer on every additional update.
                // This means inference only starts 400 ms after the user stops typing.
                'debounce: loop {
                    tokio::select! {
                        result = rx.changed() => {
                            if result.is_err() { break 'outer; }
                            // New update — restart the timer.
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(400)) => {
                            break 'debounce;
                        }
                    }
                }

                let update = match rx.borrow().clone() {
                    Some(u) => u,
                    None    => continue,
                };

                let s = settings_inf.get();
                let completer = match router_inf.lock().await.route(&update.prefix, &s) {
                    Some(c) => c,
                    None    => continue,
                };

                // Run inference on the blocking thread pool — serialised by this loop.
                let prefix_c = update.prefix.clone();
                let result = tokio::task::spawn_blocking(
                    move || completer.complete(&prefix_c, 25)
                ).await;

                match result {
                    Ok(Ok(raw)) if !raw.is_empty() => {
                        let text = model_runtime::truncate_at_sentence_boundary(raw);
                        if text.is_empty() {
                            info!("completion empty after truncation — suppressing overlay");
                            continue;
                        }
                        let event = completion_engine::CompletionEvent {
                            id: 1,
                            text: text.clone(),
                            context: CompletionContext {
                                prefix:        update.prefix,
                                suffix:        update.suffix,
                                caret_x:       update.caret_x,
                                caret_y:       update.caret_y,
                                caret_height:  update.caret_height,
                                app_bundle_id: update.app_bundle_id,
                            },
                        };
                        *current_inf.lock().await = Some(event);

                        // caret_height == 0 means AX couldn't determine caret position
                        // (Electron / terminal apps) — store the completion but skip the overlay.
                        if update.caret_height > 0.0 {
                            info!("showOverlay text={:?}", text);
                            let _ = transport_inf.lock().await
                                .send_notification("showOverlay", serde_json::json!({
                                    "x":      update.caret_x,
                                    "y":      update.caret_y,
                                    "height": update.caret_height,
                                    "text":   text,
                                }))
                                .await;
                        } else {
                            info!("completion ready (no overlay — caret bounds unavailable): {:?}", text);
                        }
                    }
                    Ok(Ok(_))  => info!("completion returned empty string"),
                    Ok(Err(e)) => warn!("completion error: {e}"),
                    Err(e)     => warn!("spawn_blocking panicked: {e}"),
                }
            }
        });
    }

    // Tell Swift whether onboarding is needed.
    {
        let needs_onboarding = !downloader.is_installed(&en_entry)
            || !settings.get().onboarding_completed;
        transport.lock().await
            .send_notification("ready", serde_json::json!({ "needsOnboarding": needs_onboarding }))
            .await?;
    }

    // Main event loop.
    loop {
        let msg = match incoming.recv().await {
            Some(m) => m,
            None => {
                info!("Swift closed the pipe — exiting");
                break;
            }
        };

        handle_message(
            msg,
            &settings,
            &exclusion_engine,
            &router,
            &current_completion,
            &ctx_tx,
            &transport,
            &telemetry,
            &downloader,
        )
        .await;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_message(
    msg: RpcMessage,
    settings: &SettingsStore,
    exclusion: &ExclusionEngine,
    router: &Arc<Mutex<LanguageRouter>>,
    current_completion: &Arc<Mutex<Option<completion_engine::CompletionEvent>>>,
    ctx_tx: &Arc<tokio::sync::watch::Sender<Option<ContextUpdate>>>,
    transport: &Arc<Mutex<IpcTransport>>,
    telemetry: &TelemetryClient,
    downloader: &Arc<ModelDownloader>,
) {
    let method = msg.method.as_deref().unwrap_or("");
    let params = msg.params.as_ref().cloned().unwrap_or(serde_json::json!({}));

    match method {
        "ping" => {
            if let Some(id) = msg.id {
                let _ = transport.lock().await.respond(id, serde_json::json!("pong")).await;
            }
        }

        "contextUpdate" => {
            let prefix          = params["prefix"].as_str().unwrap_or("").to_string();
            let suffix          = params["suffix"].as_str().unwrap_or("").to_string();
            let caret_x         = params["caretX"].as_f64().unwrap_or(0.0);
            let caret_y         = params["caretY"].as_f64().unwrap_or(0.0);
            // 0.0 = AX couldn't determine caret bounds (Electron / terminal apps).
            let caret_height    = params["caretHeight"].as_f64().unwrap_or(0.0);
            let app_bundle_id   = params["appBundleId"].as_str().unwrap_or("").to_string();
            let is_secure_field = params["isSecureField"].as_bool().unwrap_or(false);

            let s = settings.get();
            let verdict = exclusion.verdict(
                &app_bundle_id,
                is_secure_field,
                &s.app_exclusion_overrides,
                &s.messaging_toast_shown,
            );

            // Show first-activation toast for messaging apps.
            if let exclusion_engine::Verdict::DefaultOn { show_activation_toast: true, .. } = &verdict {
                let _ = settings.update(|s| { s.messaging_toast_shown.insert(app_bundle_id.clone()); });
                let _ = transport.lock().await
                    .send_notification("showMessagingToast", serde_json::json!({ "bundleId": app_bundle_id }))
                    .await;
            }

            if !verdict.completions_active() {
                // Signal debounce loop to not run inference; hide overlay.
                let _ = ctx_tx.send(None);
                let _ = transport.lock().await
                    .send_notification("hideOverlay", serde_json::json!({}))
                    .await;
                let _ = transport.lock().await
                    .send_notification("updateMenuBar", serde_json::json!({
                        "appName": app_bundle_id,
                        "active": false
                    }))
                    .await;
                return;
            }

            let _ = transport.lock().await
                .send_notification("updateMenuBar", serde_json::json!({
                    "appName": app_bundle_id,
                    "active": true
                }))
                .await;

            // Publish to the debounce channel — do NOT spawn inference here.
            let _ = ctx_tx.send(Some(ContextUpdate {
                prefix,
                suffix,
                caret_x,
                caret_y,
                caret_height,
                app_bundle_id,
            }));
        }

        "acceptCompletion" => {
            if current_completion.lock().await.take().is_some() {
                telemetry.record(TelemetryEvent::CompletionAccepted { model_id: "qwen2.5-1.5b-q4".into() });
            }
        }

        "dismissCompletion" => {
            if current_completion.lock().await.take().is_some() {
                telemetry.record(TelemetryEvent::CompletionDismissed { model_id: "qwen2.5-1.5b-q4".into() });
            }
        }

        "startModelDownload" => {
            let lang = params["language"].as_str().unwrap_or("en").to_string();
            let transport_c  = transport.clone();
            let router_c     = router.clone();
            let downloader_c = downloader.clone();

            tokio::spawn(async move {
                let entry = match ModelCatalog::default_for_language(&lang) {
                    Some(e) => e,
                    None => return,
                };

                let (progress_tx, mut progress_rx) = tokio::sync::watch::channel(
                    model_downloader::DownloadProgress::Starting { total_bytes: entry.size_bytes },
                );

                // Forward progress updates to Swift.
                let t2 = transport_c.clone();
                tokio::spawn(async move {
                    while progress_rx.changed().await.is_ok() {
                        let payload = match &*progress_rx.borrow() {
                            model_downloader::DownloadProgress::Starting { total_bytes } => {
                                serde_json::json!({ "phase": "downloading", "downloaded": 0_i64, "total": *total_bytes as i64, "progress": 0.0 })
                            }
                            model_downloader::DownloadProgress::Progress { downloaded, total } => {
                                let frac = *downloaded as f64 / (*total).max(1) as f64;
                                serde_json::json!({ "phase": "downloading", "downloaded": *downloaded as i64, "total": *total as i64, "progress": frac })
                            }
                            model_downloader::DownloadProgress::Verifying => {
                                serde_json::json!({ "phase": "verifying" })
                            }
                            model_downloader::DownloadProgress::Complete { .. } => {
                                serde_json::json!({ "phase": "complete", "progress": 1.0 })
                            }
                            model_downloader::DownloadProgress::Failed { error } => {
                                serde_json::json!({ "phase": "failed", "error": error })
                            }
                        };
                        let _ = t2.lock().await.send_notification("downloadProgress", payload).await;
                    }
                });

                match downloader_c.download(&entry, progress_tx).await {
                    Ok(path) => {
                        info!("model installed at {path:?}; loading into router");
                        match model_runtime::LlamaCppCompleter::load(&path) {
                            Ok(c) => {
                                router_c.lock().await.register(lang.clone(), Arc::new(c));
                                info!("model hot-loaded after download");
                            }
                            Err(e) => warn!("post-download model load failed: {e}"),
                        }
                    }
                    Err(e) => {
                        warn!("model download failed: {e}");
                        let _ = transport_c.lock().await
                            .send_notification("downloadProgress", serde_json::json!({ "phase": "failed", "error": e.to_string() }))
                            .await;
                    }
                }
            });
        }

        "onboardingComplete" => {
            let _ = settings.update(|s| { s.onboarding_completed = true; s.onboarding_phase = 5; });
        }

        "resetTabTypist" => {
            info!("full reset requested");
            let _ = settings_store::delete_all_data();
            if let Ok(models) = model_downloader::models_dir() {
                if models.exists() { let _ = std::fs::remove_dir_all(models); }
            }
        }

        "updateSetting" => {
            let key = params["key"].as_str().unwrap_or("");
            match key {
                "telemetryEnabled" => {
                    let enabled = params["value"].as_bool().unwrap_or(false);
                    let _ = settings.update(|s| s.telemetry_enabled = enabled);
                    telemetry.set_consent(enabled);
                }
                "disableApp" => {
                    let id = params["bundleId"].as_str().unwrap_or("").to_string();
                    let _ = settings.update(|s| { s.app_exclusion_overrides.insert(id, false); });
                }
                "enableApp" => {
                    let id = params["bundleId"].as_str().unwrap_or("").to_string();
                    let _ = settings.update(|s| { s.app_exclusion_overrides.insert(id, true); });
                }
                _ => warn!("unknown setting key: {key}"),
            }
        }

        other if !other.is_empty() => warn!("unhandled method: {other}"),
        _ => {}
    }
}
