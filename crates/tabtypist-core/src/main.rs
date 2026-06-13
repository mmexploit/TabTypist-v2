mod exclusion_engine;
mod ipc;
mod language_router;
mod model_downloader;
mod model_runtime;
mod settings_store;
mod telemetry;
mod vocab_store;

use anyhow::Result;
use exclusion_engine::ExclusionEngine;
use ipc::{IpcTransport, RpcMessage};
use language_router::LanguageRouter;
use model_downloader::{ModelCatalog, ModelDownloader, ModelEntry, ModelKind};
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
    prefix:            String,
    suffix:            String,
    caret_x:           f64,
    caret_y:           f64,
    caret_height:      f64,
    font_size:         f64,
    input_frame_x:     f64,
    input_frame_y:     f64,
    input_frame_w:     f64,
    input_frame_h:     f64,
    app_bundle_id:     String,
    app_display_name:  String, // NSRunningApplication.localizedName
    visual_context:    String, // OCR text from above the field; "" when unavailable
    clipboard_context: String, // clipboard text when user has opted in; "" otherwise
}

/// Prediction debounce derived from the last observed generation latency (adaptive
/// debounce policy). Reference bands are 15/25/55 ms for sub-1B models with a
/// 20 ms fallback; our implementation adds a heavier tier and keeps the 280 ms fallback
/// for anything slower, because on a multi-billion-param model letting keystrokes
/// pile doomed generations onto a decoder that cannot keep up drags the machine.
fn adaptive_debounce_ms(last_latency_ms: Option<u64>, fallback: u64) -> u64 {
    match last_latency_ms {
        Some(ms) if ms == 0 => fallback,
        Some(ms) if ms <= 70 => 25,
        Some(ms) if ms <= 140 => 60,
        Some(ms) if ms <= 300 => 140,
        _ => fallback,
    }
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

    // Prefer the model the user explicitly selected (stored in model_overrides),
    // then the catalog default, then any installed model.  This prevents re-showing
    // onboarding when the user picked a non-default tier.
    let installed_en_entry: Option<ModelEntry> = {
        let overridden = settings.get()
            .model_overrides
            .get("en")
            .and_then(|id| ModelCatalog::find(id))
            .filter(|e| downloader.is_installed(e));
        let default_entry = ModelCatalog::default_for_language("en")
            .filter(|e| downloader.is_installed(e));
        let any_entry = ModelCatalog::entries().into_iter().find(|e| downloader.is_installed(e));
        overridden.or(default_entry).or(any_entry)
    };
    if let Some(ref en_entry) = installed_en_entry {
        info!("loading English model from {:?}", downloader.installed_path(en_entry));
        match model_runtime::LlamaCppCompleter::load(&downloader.installed_path(en_entry)) {
            Ok(c) => {
                router.lock().await.register("en", Arc::new(c));
                info!("English model loaded");
                transport.lock().await
                    .send_notification("modelLoaded", serde_json::json!({
                        "tier": en_entry.tier,
                        "displayName": en_entry.display_name,
                    }))
                    .await?;
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

    let vocab = Arc::new(
        vocab_store::VocabStore::load(&model_downloader::models_dir().unwrap_or_default())
    );

    // None = no completion pending; Some(text) = pending text (gates telemetry + vocab record).
    let current_completion: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

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
        let vocab_inf     = vocab.clone();

        tokio::spawn(async move {
            // Last completion text we actually surfaced. Used to break the
            // post-acceptance repetition loop where the model keeps re-suggesting
            // the text the user just accepted (now part of the prefix).
            let mut last_shown: Option<String> = None;

            // Debounce before kicking off inference. A fixed value serves two masters
            // badly: a sub-1B model could fire after ~20 ms, while on a multi-billion-
            // param model (e.g. Qwen3 4B) every micro-pause launches a full decode and
            // rapid typing drags the machine. The adaptive debounce policy keys the
            // trigger to the last observed generation latency — fast model, snappy
            // trigger; slow model, calm trigger (each cancelled decode still costs a
            // setup + teardown). Until a first latency exists the conservative 280 ms
            // fallback applies. TABTYPIST_DEBOUNCE_MS pins a fixed value for tuning.
            const FALLBACK_DEBOUNCE_MS: u64 = 280;
            let fixed_debounce_ms: Option<u64> = std::env::var("TABTYPIST_DEBOUNCE_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|&ms| ms > 0);
            let mut last_latency_ms: Option<u64> = None;
            match fixed_debounce_ms {
                Some(ms) => info!("completion debounce: fixed {ms} ms"),
                None => info!("completion debounce: adaptive (fallback {FALLBACK_DEBOUNCE_MS} ms)"),
            }

            'outer: loop {
                // Block until a new context update arrives.
                if rx.changed().await.is_err() { break; }

                'process: loop {
                // Debounce: restart the timer on every additional update so inference
                // only fires once typing pauses. Collapsing keystroke bursts here is
                // what keeps a heavy model from running a decode per keystroke. A
                // superseded in-flight generation is still cancelled cooperatively.
                let debounce_ms = fixed_debounce_ms
                    .unwrap_or_else(|| adaptive_debounce_ms(last_latency_ms, FALLBACK_DEBOUNCE_MS));
                'debounce: loop {
                    tokio::select! {
                        result = rx.changed() => {
                            if result.is_err() { break 'outer; }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(debounce_ms)) => {
                            break 'debounce;
                        }
                    }
                }

                let update = match rx.borrow().clone() {
                    Some(u) => u,
                    None    => continue 'outer,
                };

                let s = settings_inf.get();
                let completer = match router_inf.lock().await.route(&update.prefix, &s) {
                    Some(c) => c,
                    None    => continue 'outer,
                };

                let prefix_c = update.prefix.clone();
                let suffix_c = update.suffix.clone();
                let base_budget = s.completion_length.token_budget();
                let max_tokens = if s.multi_line_enabled {
                    base_budget.saturating_mul(2).min(60)
                } else {
                    base_budget
                };
                let multi_line = s.multi_line_enabled;
                let length_instruction = match s.completion_length {
                    settings_store::CompletionLength::Short  => "Write only the next 3 to 7 words.".into(),
                    settings_store::CompletionLength::Medium => "Write the next 7 to 12 words.".into(),
                    settings_store::CompletionLength::Long   => "Write up to 20 words.".into(),
                };

                // Build full context for instruct-model personalisation. Clipboard is
                // relevance-gated: unless it shares
                // at least one significant token with what the user is writing, it's
                // unrelated content that steers the completion off the sentence in hand.
                let clipboard_c = if s.clipboard_context_enabled
                    && model_runtime::shares_significant_token(
                        &update.clipboard_context,
                        &update.prefix,
                    ) {
                    update.clipboard_context.clone()
                } else {
                    String::new()
                };
                let mut custom_rules = s.custom_rules_global.clone();
                if let Some(app_rules) = s.custom_rules_per_app.get(&update.app_bundle_id) {
                    if !app_rules.is_empty() {
                        if !custom_rules.is_empty() { custom_rules.push('\n'); }
                        custom_rules.push_str(app_rules);
                    }
                }
                let top_words = vocab_inf.top_words(20);
                if !top_words.is_empty() {
                    if !custom_rules.is_empty() { custom_rules.push('\n'); }
                    custom_rules.push_str(&format!("Personal vocabulary: {}", top_words.join(", ")));
                }

                let instr_ctx = model_runtime::InstrContext {
                    length_instruction,
                    visual_context:    update.visual_context.clone(),
                    clipboard_context: clipboard_c,
                    app_name:          update.app_display_name.clone(),
                    language:          detect_language(&update.prefix),
                    user_name:         s.user_name.clone(),
                    custom_rules,
                };

                info!(
                    "inference: prefix_len={} visual_ctx_len={} clipboard_len={} app={:?}",
                    update.prefix.len(),
                    instr_ctx.visual_context.len(),
                    instr_ctx.clipboard_context.len(),
                    instr_ctx.app_name,
                );
                if !instr_ctx.visual_context.is_empty() {
                    info!("visual_context text: {}", instr_ctx.visual_context.replace('\n', " | "));
                }

                // Cooperative cancellation: if a newer context update lands
                // while inference is running, flag the in-flight decode to bail after
                // its current token, then immediately regenerate from the latest state.
                // A cancelled generation leaves the KV cache valid — the rerun reuses it.
                let cancel = completer.cancel_handle();
                if let Some(flag) = &cancel {
                    flag.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                let gen_started = std::time::Instant::now();
                let mut task = tokio::task::spawn_blocking(
                    move || completer.complete_with_context(&prefix_c, &suffix_c, max_tokens, multi_line, instr_ctx)
                );
                let mut superseded = false;
                let mut channel_closed = false;
                let result = loop {
                    tokio::select! {
                        r = &mut task => break r,
                        changed = rx.changed(), if !superseded && !channel_closed => {
                            match changed {
                                Ok(()) => {
                                    superseded = true;
                                    if let Some(flag) = &cancel {
                                        flag.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                                Err(_) => channel_closed = true,
                            }
                        }
                    }
                };
                if channel_closed { break 'outer; }
                if superseded {
                    info!("superseded during inference — regenerating with latest context");
                    continue 'process;
                }
                // Feed the adaptive debounce. Only completed generations count — a
                // cancelled decode's elapsed time underestimates the model's real cost.
                last_latency_ms = Some(gen_started.elapsed().as_millis() as u64);

                // Stale-completion guard (backstop): a newer prefix can still land in the
                // window between the decode finishing and the select waking up.
                let latest_prefix = rx.borrow().as_ref().map(|u| u.prefix.clone());
                if latest_prefix.as_deref() != Some(update.prefix.as_str()) {
                    info!("discarding stale completion (prefix advanced during inference)");
                    continue 'outer;
                }

                match result {
                    Ok(Ok(raw)) if !raw.is_empty() => {
                        let text = model_runtime::truncate_at_sentence_boundary(raw);
                        if text.is_empty() {
                            info!("completion empty after truncation — suppressing overlay");
                            continue 'outer;
                        }
                        // Insertion safety gate: U+FFFD, control characters,
                        // or whitespace-only output is corruption, never a completion.
                        if !model_runtime::is_safe_to_insert(&text) {
                            info!("suppressing unsafe completion {:?}", text);
                            continue 'outer;
                        }
                        // Junk-run guard: a run of 4+
                        // identical punctuation/symbol chars ("....", "$$$$") is decode
                        // noise — unless it merely extends a divider the user already
                        // has at the caret.
                        if model_runtime::introduces_junk_punctuation_run(&text, &update.prefix) {
                            info!("suppressing junk punctuation run {:?}", text);
                            continue 'outer;
                        }
                        // Word-continuation coherence: mid-word prefixes must be
                        // finished, not abandoned for a new capitalized sentence pulled
                        // from the background context.
                        if model_runtime::breaks_word_continuation(&text, &update.prefix) {
                            info!("suppressing completion that abandons the current word {:?}", text);
                            continue 'outer;
                        }
                        // Trailing-duplication guard: never surface a completion
                        // that mostly retypes the text already after the caret — accepting
                        // it would insert a duplicate.
                        if model_runtime::duplicates_trailing_text(&text, &update.suffix) {
                            info!("suppressing completion that duplicates trailing text");
                            continue 'outer;
                        }
                        // Transcript-mimicry guard: a clock time in the completion when
                        // the user's own text has none means the model copied chat-chrome
                        // timestamps from the screen context instead of continuing the
                        // sentence — never a structurally valid suggestion.
                        if model_runtime::mimics_transcript_format(&text, &update.prefix) {
                            info!("suppressing transcript-format completion {:?}", text);
                            continue 'outer;
                        }
                        // Preceding-duplication guard: never surface a completion that
                        // retypes a sentence already sitting just before the caret —
                        // the model re-emitting the suggestion the user just accepted.
                        if model_runtime::duplicates_preceding_text(&text, &update.prefix) {
                            info!("suppressing completion that repeats preceding text");
                            continue 'outer;
                        }
                        // Repetition guard: if this is the same suggestion we just
                        // showed (folded — case/punctuation drift still counts), don't
                        // surface it again — that's the "keeps repeating" loop.
                        if last_shown.as_deref().map(model_runtime::fold_alnum)
                            == Some(model_runtime::fold_alnum(&text))
                        {
                            info!("suppressing repeated completion {:?}", text);
                            continue 'outer;
                        }
                        last_shown = Some(text.clone());
                        *current_inf.lock().await = Some(text.clone());

                        // Show overlay when caret bounds are known (standard) or when
                        // the input frame is known (Electron / popup-card mode).
                        if update.caret_height > 0.0 || update.input_frame_w > 0.0 {
                            info!("showOverlay text={:?}", text);
                            let _ = transport_inf.lock().await
                                .send_notification("showOverlay", serde_json::json!({
                                    "x":           update.caret_x,
                                    "y":           update.caret_y,
                                    "height":      update.caret_height,
                                    "fontSize":    update.font_size,
                                    "inputFrameX": update.input_frame_x,
                                    "inputFrameY": update.input_frame_y,
                                    "inputFrameW": update.input_frame_w,
                                    "inputFrameH": update.input_frame_h,
                                    "appBundleId": update.app_bundle_id,
                                    "text":        text,
                                }))
                                .await;
                        } else {
                            info!("completion ready (no overlay — caret+frame unavailable): {:?}", text);
                        }
                    }
                    Ok(Ok(_))  => info!("completion returned empty string"),
                    Ok(Err(e)) => warn!("completion error: {e}"),
                    Err(e)     => warn!("spawn_blocking panicked: {e}"),
                }
                break 'process;
                } // 'process
            }
        });
    }

    // Tell Swift whether onboarding is needed.
    {
        let needs_onboarding = installed_en_entry.is_none()
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
            &vocab,
        )
        .await;
    }

    Ok(())
}

fn should_trigger_completion(prefix: &str) -> bool {
    if prefix.trim().is_empty() {
        return false;
    }
    // Hold back right after a finished sentence (terminator + optional whitespace):
    // with zero anchor for the next thought, the model invents a generic new sentence.
    // Suggestions resume the moment the user types the first character of the next
    // sentence; abbreviations/decimals don't count as finished (ends_sentence).
    !model_runtime::ends_sentence(prefix)
}

/// Detect dominant non-Latin script in the last 200 chars of `prefix`.
/// Returns a human-readable language name for the instruct prompt, or "" for English.
fn detect_language(prefix: &str) -> String {
    let sample: String = prefix.chars().rev().take(200).collect::<String>()
        .chars().rev().collect();

    let mut ethiopic   = 0u32;
    let mut arabic     = 0u32;
    let mut cjk        = 0u32;
    let mut japanese   = 0u32;
    let mut hangul     = 0u32;
    let mut cyrillic   = 0u32;
    let mut hebrew     = 0u32;
    let mut thai       = 0u32;
    let mut devanagari = 0u32;

    for c in sample.chars() {
        let v = c as u32;
        match v {
            0x1200..=0x137F => ethiopic   += 1,
            0x0600..=0x06FF => arabic     += 1,
            0x4E00..=0x9FFF => cjk        += 1,
            0x3040..=0x30FF => japanese   += 1,
            0xAC00..=0xD7FF => hangul     += 1,
            0x0400..=0x04FF => cyrillic   += 1,
            0x0590..=0x05FF => hebrew     += 1,
            0x0E00..=0x0E7F => thai       += 1,
            0x0900..=0x097F => devanagari += 1,
            _ => {}
        }
    }

    let counts = [ethiopic, arabic, cjk, japanese, hangul, cyrillic, hebrew, thai, devanagari];
    let max_count = counts.iter().copied().max().unwrap_or(0);
    if max_count < 2 { return String::new(); }

    match counts.iter().position(|&c| c == max_count) {
        Some(0) => "Amharic".into(),
        Some(1) => "Arabic".into(),
        Some(2) => "Chinese".into(),
        Some(3) => "Japanese".into(),
        Some(4) => "Korean".into(),
        Some(5) => "Russian".into(),
        Some(6) => "Hebrew".into(),
        Some(7) => "Thai".into(),
        Some(8) => "Hindi".into(),
        _ => String::new(),
    }
}

/// True when the last line of `prefix` ends with a recognised shell-prompt suffix.
/// Used to gate completions in terminal emulators so suggestions only appear at
/// a prompt, never mid-command or inside interactive programs like vim or htop.
fn has_terminal_prompt(prefix: &str) -> bool {
    let last = prefix.lines().last().unwrap_or("");
    last.ends_with("$ ")
        || last.ends_with("> ")
        || last.ends_with("❯ ")
        || last.ends_with("% ")
        || last.ends_with("# ")
}

#[cfg(test)]
mod tests {
    use super::{has_terminal_prompt, should_trigger_completion};

    #[test]
    fn triggers_on_single_char() {
        assert!(should_trigger_completion("h"));
        assert!(should_trigger_completion("hello"));
        assert!(should_trigger_completion("  hello  "));
    }

    #[test]
    fn no_trigger_on_empty_or_whitespace() {
        assert!(!should_trigger_completion(""));
        assert!(!should_trigger_completion("   "));
        assert!(!should_trigger_completion("\t\n"));
    }

    #[test]
    fn terminal_prompt_detection_positive() {
        assert!(has_terminal_prompt("user@host:~$ "));
        assert!(has_terminal_prompt("some output\nuser@host:~$ "));
        assert!(has_terminal_prompt("~/projects > "));
        assert!(has_terminal_prompt("❯ "));
        assert!(has_terminal_prompt("root@box:~# "));
        assert!(has_terminal_prompt("(venv) % "));
    }

    #[test]
    fn terminal_prompt_detection_negative() {
        assert!(!has_terminal_prompt("git commit -m \"hello"));
        assert!(!has_terminal_prompt("user@host:~$"));    // missing trailing space
        assert!(!has_terminal_prompt(""));
        assert!(!has_terminal_prompt("some normal text"));
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_message(
    msg: RpcMessage,
    settings: &SettingsStore,
    exclusion: &ExclusionEngine,
    router: &Arc<Mutex<LanguageRouter>>,
    current_completion: &Arc<Mutex<Option<String>>>,
    ctx_tx: &Arc<tokio::sync::watch::Sender<Option<ContextUpdate>>>,
    transport: &Arc<Mutex<IpcTransport>>,
    telemetry: &TelemetryClient,
    downloader: &Arc<ModelDownloader>,
    vocab: &Arc<vocab_store::VocabStore>,
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
            // 0.0 = AX didn't report a font size; overlay falls back to caret-height estimate.
            let font_size       = params["fontSize"].as_f64().unwrap_or(0.0);
            // Focused-field bounds in Cocoa coords; zero width/height = unavailable.
            let input_frame_x   = params["inputFrameX"].as_f64().unwrap_or(0.0);
            let input_frame_y   = params["inputFrameY"].as_f64().unwrap_or(0.0);
            let input_frame_w   = params["inputFrameW"].as_f64().unwrap_or(0.0);
            let input_frame_h   = params["inputFrameH"].as_f64().unwrap_or(0.0);
            let app_bundle_id    = params["appBundleId"].as_str().unwrap_or("").to_string();
            let app_display_name = params["appDisplayName"].as_str().unwrap_or("").to_string();
            let is_secure_field  = params["isSecureField"].as_bool().unwrap_or(false);
            let visual_context   = params["visualContext"].as_str().unwrap_or("").to_string();
            let clipboard_context = params["clipboardContext"].as_str().unwrap_or("").to_string();

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

            // NOTE: we intentionally do NOT send `hideOverlay` here. An aggressive hide on
            // every contextUpdate clears the completion state in Swift (KeyCapture), so any
            // spurious AX event between showOverlay and Tab press makes Tab fall through.
            // The stale-completion guard in the debounce loop already prevents showing
            // results for an outdated prefix; the worst remaining symptom is the previous
            // overlay sitting at its old position for ~1 s while new inference runs, which
            // is far less broken than Tab not accepting.

            // Suppress suggestions when there's not enough context for a useful
            // completion: empty/whitespace prefixes, and a caret sitting right after a
            // finished sentence — there the model has no anchor and rushes into an
            // invented new sentence.
            if !should_trigger_completion(&prefix) {
                let _ = ctx_tx.send(None);
                let _ = transport.lock().await
                    .send_notification("hideOverlay", serde_json::json!({}))
                    .await;
                return;
            }

            // Terminal emulators: only trigger at a recognised shell prompt.
            // The bundle is already default-off; this second guard prevents noisy
            // suggestions mid-command when the user has explicitly enabled the app.
            if exclusion_engine::is_terminal_bundle(&app_bundle_id)
                && !has_terminal_prompt(&prefix)
            {
                let _ = ctx_tx.send(None);
                let _ = transport.lock().await
                    .send_notification("hideOverlay", serde_json::json!({}))
                    .await;
                return;
            }

            // Publish to the debounce channel — do NOT spawn inference here.
            let _ = ctx_tx.send(Some(ContextUpdate {
                prefix,
                suffix,
                caret_x,
                caret_y,
                caret_height,
                font_size,
                input_frame_x,
                input_frame_y,
                input_frame_w,
                input_frame_h,
                app_bundle_id,
                app_display_name,
                visual_context,
                clipboard_context,
            }));
        }

        "acceptCompletion" => {
            let prev = std::mem::replace(&mut *current_completion.lock().await, None);
            if let Some(text) = prev {
                telemetry.record(TelemetryEvent::CompletionAccepted { model_id: "qwen2.5-1.5b-base-q4".into() });
                vocab.record(&text);
            }
        }

        "dismissCompletion" => {
            let was_pending = std::mem::replace(&mut *current_completion.lock().await, None).is_some();
            if was_pending {
                telemetry.record(TelemetryEvent::CompletionDismissed { model_id: "qwen2.5-1.5b-base-q4".into() });
            }
        }

        "startModelDownload" => {
            let lang       = params["language"].as_str().unwrap_or("en").to_string();
            let model_id   = params["modelId"].as_str().map(|s| s.to_string());
            let custom_url = params["customUrl"].as_str().map(|s| s.to_string());
            let hf_token   = settings.get().hf_token;
            let transport_c  = transport.clone();
            let router_c     = router.clone();
            let downloader_c = downloader.clone();
            let settings_c   = settings.clone();

            tokio::spawn(async move {
                // Priority: explicit customUrl > modelId > language default.
                let entry = if let Some(url) = custom_url {
                    let filename = url.split('/').last().unwrap_or("custom-model").to_string();
                    let id = format!("custom-{}", filename.replace('.', "-"));
                    Some(ModelEntry {
                        id,
                        display_name: filename,
                        language: lang.clone(),
                        tier: "custom".to_string(),
                        model_kind: ModelKind::Instruct,
                        min_ram_gb: 0,
                        url,
                        size_bytes: 0,
                        sha256: "placeholder_custom".to_string(),
                        ed25519_signature: "placeholder_custom".to_string(),
                    })
                } else if let Some(id) = &model_id {
                    ModelCatalog::find(id).or_else(|| ModelCatalog::default_for_language(&lang))
                } else {
                    ModelCatalog::default_for_language(&lang)
                };
                let entry = match entry {
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

                let token_ref = if hf_token.is_empty() { None } else { Some(hf_token.as_str()) };
                match downloader_c.download(&entry, token_ref, progress_tx).await {
                    Ok(path) => {
                        info!("model installed at {path:?}; loading into router");
                        // Persist which model the user selected so relaunch finds it.
                        let _ = settings_c.update(|s| {
                            s.model_overrides.insert(lang.clone(), entry.id.clone());
                        });
                        match model_runtime::LlamaCppCompleter::load(&path) {
                            Ok(c) => {
                                router_c.lock().await.register(lang.clone(), Arc::new(c));
                                info!("model hot-loaded after download");
                                let _ = transport_c.lock().await
                                    .send_notification("modelLoaded", serde_json::json!({
                                        "tier": entry.tier,
                                        "displayName": entry.display_name,
                                    }))
                                    .await;
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
                "completionLength" => {
                    use settings_store::CompletionLength;
                    let preset = match params["value"].as_str().unwrap_or("") {
                        "short"  => CompletionLength::Short,
                        "medium" => CompletionLength::Medium,
                        _        => CompletionLength::Long,
                    };
                    let _ = settings.update(|s| s.completion_length = preset);
                }
                "multiLineEnabled" => {
                    let enabled = params["value"].as_bool().unwrap_or(false);
                    let _ = settings.update(|s| s.multi_line_enabled = enabled);
                }
                "userName" => {
                    let value = params["value"].as_str().unwrap_or("").to_string();
                    let _ = settings.update(|s| s.user_name = value);
                }
                "customRulesGlobal" => {
                    let value = params["value"].as_str().unwrap_or("").to_string();
                    let _ = settings.update(|s| s.custom_rules_global = value);
                }
                "customRulesApp" => {
                    let bundle_id = params["bundleId"].as_str().unwrap_or("").to_string();
                    let value     = params["value"].as_str().unwrap_or("").to_string();
                    if !bundle_id.is_empty() {
                        let _ = settings.update(|s| {
                            if value.is_empty() {
                                s.custom_rules_per_app.remove(&bundle_id);
                            } else {
                                s.custom_rules_per_app.insert(bundle_id, value);
                            }
                        });
                    }
                }
                "clipboardContextEnabled" => {
                    let enabled = params["value"].as_bool().unwrap_or(false);
                    let _ = settings.update(|s| s.clipboard_context_enabled = enabled);
                }
                "hfToken" => {
                    let value = params["value"].as_str().unwrap_or("").to_string();
                    let _ = settings.update(|s| s.hf_token = value);
                }
                _ => warn!("unknown setting key: {key}"),
            }
        }

        other if !other.is_empty() => warn!("unhandled method: {other}"),
        _ => {}
    }
}
