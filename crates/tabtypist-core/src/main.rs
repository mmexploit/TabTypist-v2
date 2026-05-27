mod completion_engine;
mod exclusion_engine;
mod ipc;
mod language_router;
mod model_downloader;
mod model_runtime;
mod settings_store;
mod telemetry;

use anyhow::{Context, Result};
use completion_engine::{CompletionContext, CompletionEngine};
use exclusion_engine::{ExclusionConfig, ExclusionEngine};
use ipc::{IpcTransport, RpcMessage};
use language_router::LanguageRouter;
use model_downloader::{ModelCatalog, ModelDownloader};
use settings_store::SettingsStore;
use std::sync::Arc;
use telemetry::{TelemetryClient, TelemetryEvent};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("tabtypist_core=info".parse().unwrap()),
        )
        .init();

    let settings = SettingsStore::load()?;
    info!("TabTypist core starting");

    // Determine sidecar path.
    let sidecar_path = sidecar_binary_path()?;
    info!("spawning sidecar at {sidecar_path:?}");

    // Spawn the Swift sidecar.
    let mut child = Command::new(&sidecar_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning sidecar {sidecar_path:?}"))?;

    let stdin = child.stdin.take().context("sidecar stdin")?;
    let stdout = child.stdout.take().context("sidecar stdout")?;

    let mut transport = IpcTransport::new(stdin);
    let mut incoming = ipc::spawn_reader(stdout);

    // Handshake: ping/pong.
    let ping_id = transport
        .request("ping", serde_json::json!({}))
        .await?;

    match incoming.recv().await {
        Some(msg) if msg.result == Some(serde_json::json!("pong")) => {
            info!("IPC handshake OK (ping id={ping_id})");
        }
        other => {
            warn!("unexpected response to ping: {other:?}");
        }
    }

    // Load the English model (if installed).
    let models_dir = model_downloader::models_dir()?;
    let ed25519_pubkey = include_bytes!("../../../Resources/ed25519_pubkey.bin");
    let downloader = ModelDownloader::new(models_dir, *ed25519_pubkey);

    let mut router = LanguageRouter::new();

    let en_entry = ModelCatalog::default_for_language("en")
        .context("no English model in catalog")?;
    if downloader.is_installed(&en_entry) {
        info!("loading English model");
        match model_runtime::LlamaCppCompleter::load(&downloader.installed_path(&en_entry)) {
            Ok(completer) => {
                router.register("en", Arc::new(completer));
                info!("English model loaded");
            }
            Err(e) => warn!("failed to load English model: {e}"),
        }
    } else {
        info!("English model not installed; completions will be unavailable until download");
    }

    let router = Arc::new(router);
    let exclusion_engine = ExclusionEngine::with_built_in();
    let settings_ref = settings.clone();
    let telemetry = TelemetryClient::new(
        settings.get().install_id.clone(),
        settings.get().telemetry_enabled,
    );

    // Track current completion so we can accept/dismiss it.
    let current_completion: Arc<Mutex<Option<completion_engine::CompletionEvent>>> =
        Arc::new(Mutex::new(None));

    let transport = Arc::new(Mutex::new(transport));

    // Tell the sidecar whether onboarding is needed (model not installed, or not yet completed).
    {
        let needs_onboarding = !downloader.is_installed(&en_entry)
            || !settings.get().onboarding_completed;
        let mut t = transport.lock().await;
        let _ = t.send_notification(
            "ready",
            serde_json::json!({ "needsOnboarding": needs_onboarding }),
        ).await;
    }

    // Main event loop.
    loop {
        let msg = match incoming.recv().await {
            Some(m) => m,
            None => {
                info!("sidecar disconnected; exiting");
                break;
            }
        };

        handle_message(
            msg,
            &settings_ref,
            &exclusion_engine,
            &router,
            &current_completion,
            &transport,
            &telemetry,
        )
        .await;
    }

    child.wait().await?;
    Ok(())
}

async fn handle_message(
    msg: RpcMessage,
    settings: &SettingsStore,
    exclusion: &ExclusionEngine,
    router: &Arc<LanguageRouter>,
    current_completion: &Arc<Mutex<Option<completion_engine::CompletionEvent>>>,
    transport: &Arc<Mutex<IpcTransport>>,
    telemetry: &TelemetryClient,
) {
    let method = msg.method.as_deref().unwrap_or("");
    let params = msg.params.as_ref().cloned().unwrap_or(serde_json::json!({}));

    match method {
        "contextUpdate" => {
            let ctx: ipc::SidecarToCore = match serde_json::from_value(serde_json::json!({
                "method": "contextUpdate",
                "params": params
            })) {
                Ok(c) => c,
                Err(e) => {
                    warn!("bad contextUpdate: {e}");
                    return;
                }
            };

            if let ipc::SidecarToCore::ContextUpdate {
                prefix,
                suffix,
                caret_x,
                caret_y,
                caret_height,
                app_bundle_id,
                is_secure_field,
            } = ctx
            {
                let s = settings.get();
                let verdict = exclusion.verdict(
                    &app_bundle_id,
                    is_secure_field,
                    &s.app_exclusion_overrides,
                    &s.messaging_toast_shown,
                );

                // Handle first-activation toast for messaging apps.
                if let exclusion_engine::Verdict::DefaultOn {
                    show_activation_toast: true,
                    ..
                } = &verdict
                {
                    if let Err(e) = settings.update(|s| {
                        s.messaging_toast_shown.insert(app_bundle_id.clone());
                    }) {
                        warn!("failed to persist toast shown: {e}");
                    }
                    let mut t = transport.lock().await;
                    let _ = t
                        .send_notification(
                            "showMessagingToast",
                            serde_json::json!({ "bundleId": app_bundle_id }),
                        )
                        .await;
                }

                if !verdict.completions_active() {
                    let mut t = transport.lock().await;
                    let _ = t
                        .send_notification("hideOverlay", serde_json::json!({}))
                        .await;
                    return;
                }

                // Route to completer.
                let completer = match router.route(&prefix, &s) {
                    Some(c) => c,
                    None => {
                        // No model loaded — tell the sidecar to hide.
                        let mut t = transport.lock().await;
                        let _ = t
                            .send_notification("hideOverlay", serde_json::json!({}))
                            .await;
                        return;
                    }
                };

                let ctx = CompletionContext {
                    prefix: prefix.clone(),
                    suffix,
                    caret_x,
                    caret_y,
                    caret_height,
                    app_bundle_id,
                };

                // Run completion inline for simplicity (the engine handles debounce).
                let prefix_clone = prefix.clone();
                let caret_x_ = caret_x;
                let caret_y_ = caret_y;
                let caret_height_ = caret_height;
                let transport_clone = transport.clone();
                let current_clone = current_completion.clone();

                tokio::spawn(async move {
                    let text = tokio::task::spawn_blocking(move || {
                        completer.complete(&prefix_clone, 25)
                    })
                    .await;

                    match text {
                        Ok(Ok(t)) if !t.is_empty() => {
                            use model_runtime::truncate_at_sentence_boundary;
                            let text = truncate_at_sentence_boundary(t);
                            let event = completion_engine::CompletionEvent {
                                id: 1,
                                text: text.clone(),
                                context: ctx,
                            };
                            *current_clone.lock().await = Some(event);

                            let mut tr = transport_clone.lock().await;
                            let _ = tr
                                .send_notification(
                                    "showOverlay",
                                    serde_json::json!({
                                        "x": caret_x_,
                                        "y": caret_y_,
                                        "height": caret_height_,
                                        "text": text
                                    }),
                                )
                                .await;
                        }
                        _ => {}
                    }
                });
            }
        }

        "acceptCompletion" => {
            let mut guard = current_completion.lock().await;
            if let Some(event) = guard.take() {
                info!("completion accepted id={}", event.id);
                telemetry.record(TelemetryEvent::CompletionAccepted {
                    model_id: "qwen2.5-1.5b-q4".to_string(),
                });
            }
        }

        "dismissCompletion" => {
            let mut guard = current_completion.lock().await;
            if guard.take().is_some() {
                info!("completion dismissed");
                telemetry.record(TelemetryEvent::CompletionDismissed {
                    model_id: "qwen2.5-1.5b-q4".to_string(),
                });
            }
        }

        "resetTabTypist" => {
            info!("reset requested — removing all TabTypist data");
            if let Err(e) = settings_store::delete_all_data() {
                warn!("reset error: {e}");
            }
            // Also remove installed model files
            if let Ok(models) = model_downloader::models_dir() {
                if models.exists() {
                    let _ = std::fs::remove_dir_all(&models);
                    info!("removed models directory at {models:?}");
                }
            }
        }

        "onboardingComplete" => {
            let _ = settings.update(|s| {
                s.onboarding_completed = true;
                s.onboarding_phase = 5; // OnboardingPhase::Done
            });
            info!("onboarding marked complete");
        }

        "startModelDownload" => {
            let lang = params
                .get("language")
                .and_then(|v| v.as_str())
                .unwrap_or("en")
                .to_string();

            let transport_clone = transport.clone();
            tokio::spawn(async move {
                let entry = match model_downloader::ModelCatalog::default_for_language(&lang) {
                    Some(e) => e,
                    None => return,
                };
                let models_dir = match model_downloader::models_dir() {
                    Ok(d) => d,
                    Err(e) => { warn!("models dir: {e}"); return; }
                };
                let ed25519_pubkey = include_bytes!("../../../Resources/ed25519_pubkey.bin");
                let downloader = model_downloader::ModelDownloader::new(models_dir, *ed25519_pubkey);

                let (progress_tx, mut progress_rx) = tokio::sync::watch::channel(
                    model_downloader::DownloadProgress::Starting { total_bytes: entry.size_bytes }
                );

                let transport_inner = transport_clone.clone();
                tokio::spawn(async move {
                    while progress_rx.changed().await.is_ok() {
                        let p = progress_rx.borrow().clone();
                        let payload = match &p {
                            model_downloader::DownloadProgress::Starting { total_bytes } => {
                                serde_json::json!({
                                    "phase": "downloading",
                                    "downloaded": 0_i64,
                                    "total": *total_bytes as i64,
                                    "progress": 0.0
                                })
                            }
                            model_downloader::DownloadProgress::Progress { downloaded, total } => {
                                let fraction = *downloaded as f64 / (*total).max(1) as f64;
                                serde_json::json!({
                                    "phase": "downloading",
                                    "downloaded": *downloaded as i64,
                                    "total": *total as i64,
                                    "progress": fraction
                                })
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
                        let mut t = transport_inner.lock().await;
                        let _ = t.send_notification("downloadProgress", payload).await;
                    }
                });

                match downloader.download(&entry, progress_tx).await {
                    Ok(_) => info!("model download complete for {lang}"),
                    Err(e) => {
                        warn!("model download failed: {e}");
                        let mut t = transport_clone.lock().await;
                        let _ = t.send_notification(
                            "downloadProgress",
                            serde_json::json!({ "phase": "failed", "error": e.to_string() }),
                        ).await;
                    }
                }
            });
        }

        "updateSetting" => {
            // sidecar sends: { key: "telemetryEnabled", value: true/false }
            if let Some(key) = params.get("key").and_then(|v| v.as_str()) {
                match key {
                    "telemetryEnabled" => {
                        let enabled = params
                            .get("value")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let _ = settings.update(|s| s.telemetry_enabled = enabled);
                        telemetry.set_consent(enabled);
                    }
                    "disableApp" => {
                        let bundle_id = params
                            .get("bundleId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let _ = settings.update(|s| {
                            s.app_exclusion_overrides.insert(bundle_id, false);
                        });
                    }
                    "enableApp" => {
                        let bundle_id = params
                            .get("bundleId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let _ = settings.update(|s| {
                            s.app_exclusion_overrides.insert(bundle_id, true);
                        });
                    }
                    _ => warn!("unknown setting key: {key}"),
                }
            }
        }

        _ if !method.is_empty() => {
            warn!("unhandled sidecar method: {method}");
        }
        _ => {}
    }
}

fn sidecar_binary_path() -> Result<std::path::PathBuf> {
    // In the app bundle: Contents/Resources/tabtypist-sidecar
    // In development: look next to this binary.
    let exe = std::env::current_exe()?;
    let dir = exe.parent().context("no parent dir for executable")?;

    // App bundle layout: MacOS/tabtypist-core → Resources/tabtypist-sidecar
    let bundle_path = dir
        .parent()
        .map(|p| p.join("Resources").join("tabtypist-sidecar"))
        .unwrap_or_default();

    if bundle_path.exists() {
        return Ok(bundle_path);
    }

    // Development: sidecar next to core binary
    let dev_path = dir.join("tabtypist-sidecar");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    // Allow override via env var
    if let Ok(p) = std::env::var("TABTYPIST_SIDECAR_PATH") {
        return Ok(std::path::PathBuf::from(p));
    }

    anyhow::bail!(
        "could not locate tabtypist-sidecar binary (tried {bundle_path:?} and {dev_path:?})"
    )
}
