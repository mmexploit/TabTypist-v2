use anyhow::{Context, Result};
use encoding_rs;
use llama_cpp_2::token::LlamaToken;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

/// A loaded model that can produce completions.
pub trait Completer: Send + Sync {
    fn complete(&self, prefix: &str, suffix: &str, max_tokens: u32) -> Result<String> {
        self.complete_ext(prefix, suffix, max_tokens, false)
    }
    fn complete_ext(
        &self,
        prefix: &str,
        suffix: &str,
        max_tokens: u32,
        multi_line: bool,
    ) -> Result<String>;
    fn complete_with_context(
        &self,
        prefix: &str,
        suffix: &str,
        max_tokens: u32,
        multi_line: bool,
        _ctx: InstrContext,
    ) -> Result<String> {
        self.complete_ext(prefix, suffix, max_tokens, multi_line)
    }
    /// Cooperative-cancellation flag for the decode loop. The caller sets it to true
    /// when a newer context update supersedes the in-flight request; the decode loop
    /// checks it per token and bails early instead of running the full budget while
    /// the next request waits behind it (cotabby's Task.isCancelled check).
    fn cancel_handle(&self) -> Option<Arc<AtomicBool>> {
        None
    }
}

// ── Inference thread ──────────────────────────────────────────────────────────
//
// LlamaContext<'model> borrows from LlamaModel, so they cannot both live in a
// struct field without unsafe self-referential tricks.  Owning all three
// (backend, model, context) inside a dedicated thread's local scope avoids the
// lifetime problem entirely while also keeping a persistent KV cache that
// survives across completion calls.

/// Optional context injected into instruct prompts (priority order from ADR 0006).
#[derive(Debug, Default, Clone)]
pub struct InstrContext {
    pub length_instruction: String,
    pub visual_context: String,    // OCR text from screen above the field
    pub clipboard_context: String, // opt-in clipboard text
    pub app_name: String,
    pub language: String,
    pub user_name: String,
    pub custom_rules: String,
}

struct InferRequest {
    prefix: String,
    suffix: String,
    max_tokens: u32,
    multi_line: bool,
    is_instruct: bool,
    instr_ctx: InstrContext,
    cancel: Arc<AtomicBool>,
    reply_tx: mpsc::SyncSender<Result<String>>,
}

pub struct LlamaCppCompleter {
    request_tx: mpsc::SyncSender<InferRequest>,
    /// True when the loaded model is an instruct-tuned model (detected from filename).
    pub is_instruct: bool,
    cancel: Arc<AtomicBool>,
}

impl LlamaCppCompleter {
    pub fn load(model_path: &Path) -> Result<Self> {
        let is_instruct = is_instruct_model(model_path);
        let (request_tx, request_rx) = mpsc::sync_channel::<InferRequest>(1);
        let model_path = model_path.to_owned();
        std::thread::spawn(move || {
            if let Err(e) = inference_thread(request_rx, model_path) {
                tracing::error!("inference thread exited: {e}");
            }
        });
        Ok(Self {
            request_tx,
            is_instruct,
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }
}

/// Detect instruct models from common filename markers.
fn is_instruct_model(path: &Path) -> bool {
    let name = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    name.contains("-it") || name.contains("-instruct") || name.contains("instruct")
        || name.contains("smollm") // SmolLM2 is always instruct
        || name.contains("-chat")
}

impl Completer for LlamaCppCompleter {
    fn complete_ext(
        &self,
        prefix: &str,
        suffix: &str,
        max_tokens: u32,
        multi_line: bool,
    ) -> Result<String> {
        self.complete_with_context(prefix, suffix, max_tokens, multi_line, InstrContext::default())
    }

    fn complete_with_context(
        &self,
        prefix: &str,
        suffix: &str,
        max_tokens: u32,
        multi_line: bool,
        instr_ctx: InstrContext,
    ) -> Result<String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.request_tx
            .send(InferRequest {
                prefix: prefix.to_owned(),
                suffix: suffix.to_owned(),
                max_tokens,
                multi_line,
                is_instruct: self.is_instruct,
                instr_ctx,
                cancel: self.cancel.clone(),
                reply_tx,
            })
            .context("inference thread disconnected")?;
        reply_rx.recv().context("inference thread dropped reply")?
    }

    fn cancel_handle(&self) -> Option<Arc<AtomicBool>> {
        Some(self.cancel.clone())
    }
}

const N_CTX: u32 = 2048;

fn inference_thread(rx: mpsc::Receiver<InferRequest>, model_path: PathBuf) -> Result<()> {
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::model::params::LlamaModelParams;

    let backend = LlamaBackend::init()?;
    let model_params = LlamaModelParams::default().with_n_gpu_layers(99);
    let model = llama_cpp_2::model::LlamaModel::load_from_file(&backend, &model_path, &model_params)
        .with_context(|| format!("loading model from {}", model_path.display()))?;

    // n_batch (logical batch) must be >= the largest single decode we submit.
    // We prefill the entire token stream in one batch, which can be up to N_CTX
    // tokens, so keep n_batch == N_CTX. llama.cpp splits this into physical
    // micro-batches of n_ubatch (default 512) internally for causal decoding.
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(Some(NonZeroU32::new(N_CTX).unwrap()))
        .with_n_batch(N_CTX);
    let mut ctx = model.new_context(&backend, ctx_params)?;

    // Tokens currently committed to the KV cache (prefix-only, no FIM framing).
    let mut kv_tokens: Vec<LlamaToken> = Vec::new();

    while let Ok(req) = rx.recv() {
        let InferRequest {
            prefix,
            suffix,
            max_tokens,
            multi_line,
            is_instruct,
            instr_ctx,
            cancel,
            reply_tx,
        } = req;
        let result = if is_instruct {
            do_complete_instruct(&model, &mut ctx, &mut kv_tokens, &prefix, max_tokens, multi_line, &instr_ctx, &cancel)
        } else {
            do_complete(&model, &mut ctx, &mut kv_tokens, &prefix, &suffix, max_tokens, multi_line, &instr_ctx, &cancel)
        };
        let _ = reply_tx.send(result);
    }
    Ok(())
}

// ── Core completion ───────────────────────────────────────────────────────────

fn do_complete(
    model: &llama_cpp_2::model::LlamaModel,
    ctx: &mut llama_cpp_2::context::LlamaContext,
    kv_tokens: &mut Vec<LlamaToken>,
    prefix: &str,
    suffix: &str,
    max_tokens: u32,
    multi_line: bool,
    instr_ctx: &InstrContext,
    cancel: &AtomicBool,
) -> Result<String> {
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::AddBos;

    // Window the prompt to the recent tail (cotabby's truncatedPromptPrefix): sending
    // an entire editor buffer hurts prefill latency with little quality gain, and long
    // stale context steers the completion away from the local continuation. The full
    // prefix is still used for echo suppression / spacing in normalize_completion.
    //
    // In plain-continuation mode a conditioning preface (persona, style, context —
    // described as facts, never commanded) precedes the caret text, separated by a
    // blank line with no label the model could copy; the prefix stays the final bytes
    // of the prompt so generation continues from where the user stopped and the KV
    // common-prefix reuse keeps the static preface cached. FIM framing is positional,
    // so the preface is skipped when a suffix is present.
    let windowed = window_prefix(prefix);
    let prompt_prefix = if suffix.is_empty() {
        let preface = base_preface(instr_ctx);
        if preface.is_empty() {
            windowed
        } else {
            format!("{preface}\n\n{windowed}")
        }
    } else {
        windowed
    };
    let new_tokens = model
        .str_to_token(&prompt_prefix, AddBos::Always)
        .context("tokenizing prefix")?;

    // Prefill budget: positions we may decode while still leaving KV room for the
    // tokens we plan to generate. Mirrors the cap in build_token_stream so the fast
    // path can never push `pos` past the context and overflow the KV cache.
    let max_prefix = (N_CTX as usize).saturating_sub(max_tokens as usize + 4);

    // KV reuse via longest-common-token-prefix (cotabby obtainAutocompleteSequence):
    // trim the cache to the shared prefix and decode only the remainder. Unlike the
    // old strict-forward-extension check this also salvages most of the cache on
    // deletions, mid-word edits, and prefix-window slides. Always leave at least one
    // token to decode so the sampler has fresh logits.
    let common = common_prefix_len(kv_tokens, &new_tokens);
    let reusable = if suffix.is_empty() && new_tokens.len() <= max_prefix {
        common.min(new_tokens.len().saturating_sub(1))
    } else {
        0 // FIM mode and over-budget prompts always re-prefill from scratch.
    };

    let (mut pos, sample_idx): (i32, i32) = if reusable > 0
        && matches!(
            ctx.clear_kv_cache_seq(Some(0), Some(reusable as u32), None),
            Ok(true)
        )
    {
        // Reflect the trim immediately so an error mid-decode can't leave kv_tokens
        // claiming positions that were already evicted from the cache.
        kv_tokens.truncate(reusable);
        let delta = &new_tokens[reusable..];
        let start = reusable as i32;
        let mut batch = LlamaBatch::new(delta.len().max(1), 1);
        for (i, &tok) in delta.iter().enumerate() {
            batch.add(tok, start + i as i32, &[0], i == delta.len() - 1)?;
        }
        ctx.decode(&mut batch).context("delta prefill")?;
        *kv_tokens = new_tokens.clone();
        (new_tokens.len() as i32, delta.len() as i32 - 1)
    } else {
        // Cold path: nothing reusable (first call / FIM mode / over-budget prompt).
        ctx.clear_kv_cache();
        kv_tokens.clear();

        let token_stream =
            build_token_stream(model, &new_tokens, suffix, max_tokens as usize)?;
        if token_stream.is_empty() {
            return Ok(String::new());
        }

        let last_idx = token_stream.len() - 1;
        let mut batch = LlamaBatch::new(token_stream.len().max(512), 1);
        for (i, &tok) in token_stream.iter().enumerate() {
            batch.add(tok, i as i32, &[0], i == last_idx)?;
        }
        ctx.decode(&mut batch).context("full prefill")?;

        // FIM framing tokens pollute the sequence, so don't cache them.
        if suffix.is_empty() {
            *kv_tokens = token_stream.clone();
        }

        (token_stream.len() as i32, last_idx as i32)
    };

    let fim_pad_id = resolve_token(model, "<|fim_pad|>");
    let endoftext_id = resolve_token(model, "<|endoftext|>");

    let mut sampler = completion_sampler();

    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut result = String::new();
    let mut tokens_emitted = 0usize;

    let mut token = sampler.sample(ctx, sample_idx);
    sampler.accept(token);

    for _ in 0..max_tokens {
        // Cooperative cancellation: a newer keystroke superseded this request, so
        // stop decoding and free the inference thread for it. The KV trim below
        // still runs, leaving the cache valid for the next (reusing) request.
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if token == model.token_eos() {
            break;
        }
        if fim_pad_id.map_or(false, |id| token == id) {
            break;
        }
        if endoftext_id.map_or(false, |id| token == id) {
            break;
        }

        let piece = model.token_to_piece(token, &mut decoder, false, None)?;
        if !piece.is_empty() {
            result.push_str(&piece);
            tokens_emitted += 1;
            if multi_line {
                if result.contains("\n\n") { break; }
            } else if result.contains('\n') {
                break;
            } else if tokens_emitted >= SENTENCE_STOP_MIN_TOKENS && ends_sentence(&result) {
                break;
            }
        }

        let mut next = LlamaBatch::new(1, 1);
        next.add(token, pos, &[0], true)?;
        ctx.decode(&mut next).context("autoregressive decode")?;
        pos += 1;

        token = sampler.sample(ctx, 0);
        sampler.accept(token);
    }

    // Trim autoregressive tokens out of the KV cache.  The next call's fast-path
    // check compares against kv_tokens (prefix only), so the cache must match.
    let _ = ctx.clear_kv_cache_seq(Some(0), Some(kv_tokens.len() as u32), None);

    let normalized = normalize_completion(result, prefix);
    // Never hand back the screen/clipboard context we fed in as a "fact"; base models
    // sometimes copy the preface verbatim instead of continuing the user's text.
    let normalized = suppress_context_copy(normalized, instr_ctx);
    tracing::debug!(
        "completion kv_reused={} cached={} multi_line={} normalized_len={}",
        reusable,
        kv_tokens.len(),
        multi_line,
        normalized.len()
    );
    Ok(if multi_line {
        truncate_at_blank_line(normalized)
    } else {
        truncate_at_sentence_boundary(normalized)
    })
}

// ── Instruct inference path ───────────────────────────────────────────────────

fn do_complete_instruct(
    model: &llama_cpp_2::model::LlamaModel,
    ctx: &mut llama_cpp_2::context::LlamaContext,
    kv_tokens: &mut Vec<LlamaToken>,
    prefix: &str,
    max_tokens: u32,
    multi_line: bool,
    instr_ctx: &InstrContext,
    cancel: &AtomicBool,
) -> Result<String> {
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::AddBos;

    // Build the instruction body. Structure adapted from cotabby's LlamaPromptRenderer,
    // which drives the same gemma-4-E2B model: one richly-structured plain-text block
    // with explicit "this is autocomplete, not chat" framing, the prefix labelled and
    // placed LAST as "Text before caret:", and a final cue that the next line must begin
    // with the continuation. Critically there is NO assistant prefill — the model
    // generates a fresh turn; the explicit framing (not the prefix's position in a chat
    // turn) is what stops it from replying conversationally or echoing the name.
    let _ = max_tokens; // length governed by token budget, not an in-prompt word range
    // Single instruction paragraph (verified against gemma-4-E2B): it makes the model
    // EXTEND the user's sentence — even a complete one, with a natural next clause —
    // using the background only for topic, never answering it. Heavier multi-rule
    // framing made the model either echo the prefix or reply to the conversation.
    let mut sections: Vec<String> = vec![
        "You are an inline autocomplete inside a text field. Continue the user's text \
         from EXACTLY where they stopped, writing only the characters that come next. \
         Keep it SHORT: finish the current word or clause and STOP — at most one short \
         sentence. Never ramble or chain clauses together with repeated 'and', 'so', or \
         commas. If they are mid-word or mid-sentence, finish it naturally. If their \
         sentence already looks complete, or you have nothing specific to add from the \
         background, offer just a brief transition the user would likely type next — a \
         word or two such as 'and then', 'which', 'because', 'so that' — rather than \
         inventing facts or repeating yourself. Use the background only as a loose hint \
         for the topic; never answer, reply to, quote, or copy it. Output only the \
         continuation, nothing else.".into(),
    ];

    if !instr_ctx.user_name.is_empty() {
        sections.push(format!(
            "The user's name is {} (use it only if they were already writing it).",
            instr_ctx.user_name
        ));
    }
    if !instr_ctx.custom_rules.is_empty() {
        sections.push(String::new());
        sections.push("Style preferences (apply only when they fit the continuation naturally):".into());
        for rule in instr_ctx.custom_rules.lines().filter(|l| !l.trim().is_empty()) {
            sections.push(format!("- {}", rule.trim()));
        }
    }

    sections.push(String::new());
    sections.push("Background (reference only — do NOT reply to any of this):".into());
    if !instr_ctx.app_name.is_empty() {
        sections.push(format!("The user is typing in {}.", instr_ctx.app_name));
    }
    if !instr_ctx.visual_context.is_empty() {
        // Low-noise background: flatten to one line and keep only the tail (nearest the
        // caret = most relevant) so a wall of screen text can't dominate the prompt or
        // be copied wholesale. Labelled as a loose hint, not "the message to reply to".
        let screen = instr_ctx.visual_context.trim();
        let capped: String = if screen.chars().count() > 500 {
            screen.chars().rev().take(500).collect::<Vec<char>>().into_iter().rev().collect()
        } else {
            screen.to_string()
        };
        sections.push("Nearby on screen (loose topic hint only):".into());
        sections.push(capped.replace('\n', " "));
    }
    if !instr_ctx.clipboard_context.is_empty() {
        let clip = &instr_ctx.clipboard_context;
        let tail = if clip.len() > 200 { &clip[clip.len() - 200..] } else { clip.as_str() };
        sections.push("Clipboard:".into());
        sections.push(tail.to_string());
    }
    if !instr_ctx.language.is_empty() {
        sections.push(format!("Write the continuation in {}.", instr_ctx.language));
    }

    sections.push(String::new());
    sections.push("The user has typed (continue from the end, do not repeat it):".into());
    // Window to the recent word tail (cotabby): keeps the prompt small and stops long
    // stale context from steering the continuation. Placed LAST so the KV reuse below
    // keeps the static instruction + background cached across keystrokes.
    sections.push(window_prefix(prefix));
    let body = sections.join("\n");

    // Wrap the body in one user turn using the model's ACTUAL chat-control tokens.
    // We detect the family by which marker is a real single token (see single_token):
    // emitting the wrong markers as literal text is what fed gemma-4 garbage before.
    // str_to_token parses special tokens (parse_special=true), so the correct literal
    // strings map to the real control tokens. We format manually rather than via the
    // embedded Jinja template — llama-cpp-2's engine returns ffi error -1 on gemma-4's
    // template and chokes on Qwen3's thinking-mode conditionals.
    let prompt = if single_token(model, "<|turn>").is_some() {
        // Gemma 4 (gemma4 arch): <|turn>role … <turn|>.
        format!("<|turn>user\n{body}<turn|>\n<|turn>model\n")
    } else if single_token(model, "<start_of_turn>").is_some() {
        // Gemma 2 / 3.
        format!("<start_of_turn>user\n{body}<end_of_turn>\n<start_of_turn>model\n")
    } else {
        // ChatML (Qwen3, SmolLM2, and most other instruct GGUFs).
        format!("<|im_start|>user\n{body}<|im_end|>\n<|im_start|>assistant\n")
    };

    // Tokenise the full prompt.
    let new_tokens = model.str_to_token(&prompt, AddBos::Always)
        .context("tokenizing instruct prompt")?;

    let max_ctx = N_CTX as usize - max_tokens as usize - 4;
    let was_truncated = new_tokens.len() > max_ctx;
    let tokens: Vec<LlamaToken> = if was_truncated {
        new_tokens[new_tokens.len() - max_ctx..].to_vec()
    } else {
        new_tokens
    };

    // KV reuse (cotabby): the instruct prompt's static head — instruction, rules,
    // background — is identical across keystrokes, and the user prefix sits at the
    // very end. Trim the cache to the longest common token prefix with the previous
    // prompt and decode only the changed tail instead of re-prefilling everything.
    let reusable = if was_truncated {
        0 // front-truncated prompts shift all positions; nothing lines up.
    } else {
        common_prefix_len(kv_tokens, &tokens).min(tokens.len().saturating_sub(1))
    };

    let last_idx = tokens.len().saturating_sub(1);
    if reusable > 0
        && matches!(
            ctx.clear_kv_cache_seq(Some(0), Some(reusable as u32), None),
            Ok(true)
        )
    {
        kv_tokens.truncate(reusable);
        let delta = &tokens[reusable..];
        let mut batch = LlamaBatch::new(delta.len().max(1), 1);
        for (i, &tok) in delta.iter().enumerate() {
            batch.add(tok, (reusable + i) as i32, &[0], i == delta.len() - 1)?;
        }
        ctx.decode(&mut batch).context("instruct delta prefill")?;
    } else {
        ctx.clear_kv_cache();
        kv_tokens.clear();
        let mut batch = LlamaBatch::new(tokens.len().max(512), 1);
        for (i, &tok) in tokens.iter().enumerate() {
            batch.add(tok, i as i32, &[0], i == last_idx)?;
        }
        ctx.decode(&mut batch).context("instruct prefill")?;
    }
    *kv_tokens = tokens.clone();

    let fim_pad_id = resolve_token(model, "<|fim_pad|>");
    let endoftext_id = resolve_token(model, "<|endoftext|>");
    // Turn terminators — only treat as stops when they are real single tokens for
    // this model, so a marker that splits into junk pieces can't false-match.
    let stop_tokens: Vec<LlamaToken> = [
        single_token(model, "<turn|>"),       // gemma-4
        single_token(model, "<end_of_turn>"), // gemma-2/3
        single_token(model, "<|im_end|>"),    // chatml
    ]
    .into_iter()
    .flatten()
    .collect();

    // Same sampler as the base path (cotabby uses one sampling config for every
    // request). The connector-loop failure mode is handled by the word-stutter guard
    // below rather than a heavy repeat penalty, which distorted word choice.
    let mut sampler = completion_sampler();

    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut result = String::new();
    let mut tokens_emitted = 0usize;
    let mut pos = tokens.len() as i32;

    // Confidence tracking — cotabby's text-stream gate (LlamaGenerationOptions.confidenceFloor).
    // Accumulate the average per-token log-probability of the emitted tokens. When the model
    // runs past a natural stopping point and keeps the sentence going by inventing/chaining,
    // its per-token confidence falls; a completion whose average drops below the floor is
    // suppressed wholesale rather than shown as a run-on. Tunable via TABTYPIST_CONFIDENCE_FLOOR.
    let mut sum_lp = 0f64;
    let mut n_lp = 0usize;

    let mut token = sampler.sample(ctx, last_idx as i32);
    let mut cur_lp = token_logprob(ctx, last_idx as i32, token);
    sampler.accept(token);

    for _ in 0..max_tokens {
        // Cooperative cancellation — see do_complete. The KV trim below still runs.
        if cancel.load(Ordering::Relaxed) { break; }
        if token == model.token_eos() { break; }
        if fim_pad_id.map_or(false, |id| token == id) { break; }
        if endoftext_id.map_or(false, |id| token == id) { break; }
        if stop_tokens.contains(&token) { break; }

        // The token is part of the output — fold its confidence into the running average.
        sum_lp += cur_lp as f64;
        n_lp += 1;

        let piece = model.token_to_piece(token, &mut decoder, false, None)?;
        if !piece.is_empty() {
            result.push_str(&piece);
            tokens_emitted += 1;
            // Hard stop on a word stutter ("and and", "the the"): the model has started
            // looping. Drop the duplicate and stop — a single connector left behind is a
            // fine transitional completion ("…and"); a repeated one never is.
            if let Some(trimmed) = strip_trailing_word_stutter(&result) {
                result = trimmed;
                break;
            }
            if multi_line {
                if result.contains("\n\n") { break; }
            } else if result.contains('\n') {
                break;
            } else if tokens_emitted >= SENTENCE_STOP_MIN_TOKENS && ends_sentence(&result) {
                break;
            }
        }

        let mut next = LlamaBatch::new(1, 1);
        next.add(token, pos, &[0], true)?;
        ctx.decode(&mut next).context("instruct autoregressive decode")?;
        pos += 1;

        token = sampler.sample(ctx, 0);
        cur_lp = token_logprob(ctx, 0, token);
        sampler.accept(token);
    }

    // Trim generated tokens out of the KV cache so the next call's common-prefix
    // check (against kv_tokens, prompt-only) matches the cache's real contents.
    let _ = ctx.clear_kv_cache_seq(Some(0), Some(kv_tokens.len() as u32), None);

    // Confidence floor: suppress a low-confidence (rambling/invented) completion entirely.
    let floor = std::env::var("TABTYPIST_CONFIDENCE_FLOOR")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(DEFAULT_CONFIDENCE_FLOOR);
    let avg_lp = if n_lp > 0 { sum_lp / n_lp as f64 } else { 0.0 };
    if std::env::var("TABTYPIST_LOG_PROMPT").is_ok() {
        tracing::info!(
            "CONFIDENCE: avg_logprob={:.3} over {} tokens (floor={:.3}) raw={:?}",
            avg_lp, n_lp, floor, result
        );
    }
    if n_lp > 0 && avg_lp < floor {
        return Ok(String::new());
    }

    let mut normalized = normalize_completion(result, prefix);
    // Same guard as the base path: suppress completions that copy the injected
    // background context (screen OCR / clipboard) rather than answering the caret.
    normalized = suppress_context_copy(normalized, instr_ctx);

    // Spacing fix (instruct path only): chat models emit the continuation as a fresh
    // message, so a new-word continuation arrives WITHOUT the joining space (e.g.
    // prefix "…properly" + "and I am…" → "…properlyand I am…"). If the prefix ends on a
    // word character and the continuation starts on one too, insert the missing space.
    // (The base/FIM path handles its own spacing and must not get this.)
    let prefix_ends_word = prefix.chars().last().map_or(false, |c| c.is_alphanumeric());
    let comp_starts_word = normalized.chars().next().map_or(false, |c| c.is_alphanumeric());
    if prefix_ends_word && comp_starts_word {
        normalized.insert(0, ' ');
    }

    tracing::debug!("instruct completion len={}", normalized.len());
    Ok(if multi_line {
        truncate_at_blank_line(normalized)
    } else {
        truncate_at_sentence_boundary(normalized)
    })
}

// ── Token stream construction ─────────────────────────────────────────────────

fn build_token_stream(
    model: &llama_cpp_2::model::LlamaModel,
    prefix_tokens: &[LlamaToken],
    suffix: &str,
    max_tokens: usize,
) -> Result<Vec<LlamaToken>> {
    use llama_cpp_2::model::AddBos;

    if suffix.is_empty() {
        let mut tokens = prefix_tokens.to_vec();
        let max_prefix = N_CTX as usize - max_tokens - 4;
        if tokens.len() > max_prefix {
            let drop = tokens.len() - max_prefix;
            tokens.drain(..drop);
        }
        return Ok(tokens);
    }

    // Fill-in-the-Middle: <fim_prefix> prefix <fim_suffix> suffix <fim_middle>
    let fim_prefix_id = resolve_token(model, "<|fim_prefix|>");
    let fim_suffix_id = resolve_token(model, "<|fim_suffix|>");
    let fim_middle_id = resolve_token(model, "<|fim_middle|>");

    if let (Some(fp), Some(fs), Some(fm)) = (fim_prefix_id, fim_suffix_id, fim_middle_id) {
        let mut prefix_tokens = prefix_tokens.to_vec();
        let mut suffix_tokens = model
            .str_to_token(suffix, AddBos::Never)
            .context("tokenizing suffix (FIM)")?;

        const SUFFIX_CAP: usize = 256;
        if suffix_tokens.len() > SUFFIX_CAP {
            suffix_tokens.truncate(SUFFIX_CAP);
        }

        let prefix_budget =
            N_CTX as usize - max_tokens - 3 - suffix_tokens.len() - 4;
        if prefix_tokens.len() > prefix_budget {
            let drop = prefix_tokens.len() - prefix_budget;
            prefix_tokens.drain(..drop);
        }

        let mut tokens =
            Vec::with_capacity(1 + prefix_tokens.len() + 1 + suffix_tokens.len() + 1);
        tokens.push(fp);
        tokens.extend_from_slice(&prefix_tokens);
        tokens.push(fs);
        tokens.extend_from_slice(&suffix_tokens);
        tokens.push(fm);
        Ok(tokens)
    } else {
        tracing::warn!("FIM tokens not found in vocab; falling back to prefix-only");
        let mut tokens = prefix_tokens.to_vec();
        let max_prefix = N_CTX as usize - max_tokens - 4;
        if tokens.len() > max_prefix {
            let drop = tokens.len() - max_prefix;
            tokens.drain(..drop);
        }
        Ok(tokens)
    }
}

fn resolve_token(
    model: &llama_cpp_2::model::LlamaModel,
    s: &str,
) -> Option<LlamaToken> {
    use llama_cpp_2::model::AddBos;
    model
        .str_to_token(s, AddBos::Never)
        .ok()
        .and_then(|t| t.into_iter().next())
}

/// Returns the token id for `s` ONLY if it maps to exactly one vocab token, i.e.
/// `s` is a real special control token in this model rather than ordinary text
/// that splits into several pieces. Used to detect a model's chat format and its
/// turn-terminator: e.g. gemma-4 has `<|turn>`/`<turn|>` as single tokens, while
/// `<start_of_turn>` (the gemma-2/3 marker) splits into 7 junk tokens there.
fn single_token(
    model: &llama_cpp_2::model::LlamaModel,
    s: &str,
) -> Option<LlamaToken> {
    use llama_cpp_2::model::AddBos;
    model
        .str_to_token(s, AddBos::Never)
        .ok()
        .filter(|t| t.len() == 1)
        .map(|t| t[0])
}

// ── Sentence-boundary helpers ─────────────────────────────────────────────────

/// If `text` ends with the same word twice in a row ("and and", "the the"), return it
/// with that trailing duplicate removed; otherwise `None`. Case-insensitive, alphabetic
/// words only (so "ha ha" or a deliberate "no no" of two different runs aren't special-
/// cased away — both words must be identical and the duplicate is the final token run).
fn strip_trailing_word_stutter(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 2 {
        return None;
    }
    let last = words[words.len() - 1];
    let prev = words[words.len() - 2];
    if last.eq_ignore_ascii_case(prev) && last.chars().all(|c| c.is_alphabetic()) {
        // Cut at the start of the final (duplicate) occurrence.
        if let Some(pos) = text.rfind(last) {
            return Some(text[..pos].trim_end().to_string());
        }
    }
    None
}

/// Default average per-token log-probability below which an instruct completion is
/// suppressed as low-confidence — cotabby's `LlamaGenerationOptions.confidenceFloor`.
/// Defaults to disabled (`-inf`), matching cotabby: calibration on gemma-4-E2B (see
/// examples/calibrate_confidence.rs) showed that with our near-greedy sampling the model
/// stays confident (~-0.3 avg) even when inventing, so a *good* completion and pure noise
/// score the same — no fixed floor separates them. Kept wired and tunable via
/// TABTYPIST_CONFIDENCE_FLOOR (e.g. -0.5) for experimentation, but off by default.
const DEFAULT_CONFIDENCE_FLOOR: f64 = f64::NEG_INFINITY;

/// Log-probability the model assigned to `token` at output position `idx`, computed as a
/// numerically-stable log-softmax over the raw logits (`logit[t] - logsumexp(logits)`).
fn token_logprob(
    ctx: &llama_cpp_2::context::LlamaContext,
    idx: i32,
    token: LlamaToken,
) -> f32 {
    let logits = ctx.get_logits_ith(idx);
    let id = token.0 as usize;
    if id >= logits.len() {
        return 0.0;
    }
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sumexp = 0f32;
    for &l in logits {
        sumexp += (l - max).exp();
    }
    let logsumexp = max + sumexp.ln();
    logits[id] - logsumexp
}

/// Minimum tokens generated before the sentence-boundary early stop may fire, guarding
/// against degenerate instant stops like a lone leading period (cotabby DecodeStopPolicy).
const SENTENCE_STOP_MIN_TOKENS: usize = 2;

/// Shared sampler chain for both inference paths — cotabby's shipped SamplingConfig:
/// gentle repeat penalty (1.05; heavier values distort word choice mid-sentence),
/// top-k 20 → top-p 0.7 → min-p 0.08 → temp 0.1, then dist with a FIXED seed so the
/// same context always produces the same ghost text (their defaultSamplerSeed).
fn completion_sampler() -> llama_cpp_2::sampling::LlamaSampler {
    use llama_cpp_2::sampling::LlamaSampler;
    LlamaSampler::chain_simple([
        LlamaSampler::penalties(64, 1.05, 0.0, 0.0),
        LlamaSampler::top_k(20),
        LlamaSampler::top_p(0.7, 1),
        LlamaSampler::min_p(0.08, 1),
        LlamaSampler::temp(0.1),
        LlamaSampler::dist(0x00C0_FFEE),
    ])
}

/// Length of the longest shared token prefix between the cached and new prompt.
fn common_prefix_len(a: &[LlamaToken], b: &[LlamaToken]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

/// Character/word caps for the prompt window (cotabby SuggestionConfiguration.standard).
const PREFIX_WINDOW_CHARS: usize = 1000;
const PREFIX_WINDOW_WORDS: usize = 50;

/// Keep only the latest short word tail of the prefix for the prompt (cotabby
/// truncatedPromptPrefix): last 1000 chars, then the last 50 whitespace-separated
/// words joined by single spaces. This bounds prefill latency in long documents,
/// stops stale context from steering output, and (by dropping trailing whitespace)
/// makes the prompt end at a clean word boundary so the model's first token decides
/// mid-word continuation vs new word.
pub fn window_prefix(prefix: &str) -> String {
    let char_window: String = {
        let count = prefix.chars().count();
        prefix
            .chars()
            .skip(count.saturating_sub(PREFIX_WINDOW_CHARS))
            .collect()
    };
    let words: Vec<&str> = char_window.split_whitespace().collect();
    if words.is_empty() {
        return char_window;
    }
    words[words.len().saturating_sub(PREFIX_WINDOW_WORDS)..].join(" ")
}

/// Conditioning preface for the base-model continuation path (cotabby
/// BaseCompletionPromptRenderer). A base model has no instruction-following channel —
/// it conditions on description, it does not obey commands — so persona, style,
/// language, and supporting context are folded into short factual lines. The app name
/// is deliberately excluded: app/window metadata biases a base model toward
/// code/numbers over prose. Returns "" when there is nothing to condition on.
pub fn base_preface(ctx: &InstrContext) -> String {
    fn tail_chars(s: &str, n: usize) -> String {
        let count = s.chars().count();
        s.chars().skip(count.saturating_sub(n)).collect()
    }
    fn head_chars(s: &str, n: usize) -> String {
        s.chars().take(n).collect()
    }

    let mut lines: Vec<String> = Vec::new();

    let name = ctx.user_name.trim();
    if !name.is_empty() {
        lines.push(head_chars(&format!("Written by {name}."), 200));
    }

    // Style rules condition the voice; the personal-vocabulary line (built upstream in
    // main.rs) is rendered as its own descriptive fact rather than a style command.
    let mut style_rules: Vec<&str> = Vec::new();
    let mut vocab: Option<&str> = None;
    for line in ctx.custom_rules.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(v) = line.strip_prefix("Personal vocabulary: ") {
            vocab = Some(v);
        } else {
            style_rules.push(line);
        }
    }
    if !style_rules.is_empty() {
        lines.push(head_chars(
            &format!("Writing style: {}.", style_rules.join(", ")),
            300,
        ));
    }
    if let Some(v) = vocab {
        lines.push(head_chars(&format!("Often uses the words: {v}."), 300));
    }

    let lang = ctx.language.trim();
    if !lang.is_empty() {
        lines.push(format!("Written in {lang}."));
    }

    let clip = ctx.clipboard_context.trim();
    if !clip.is_empty() {
        lines.push(format!(
            "On the clipboard: {}",
            tail_chars(clip, 400).replace('\n', " ")
        ));
    }

    // Keep the tail of the OCR text — physically closest to the input field, so most
    // relevant — and flatten newlines so a wall of screen text reads as one fact.
    let screen = ctx.visual_context.trim();
    if !screen.is_empty() {
        lines.push(format!(
            "Nearby on screen: {}",
            tail_chars(screen, 500).replace('\n', " ")
        ));
    }

    lines.join("\n")
}

/// Lowercased abbreviations whose trailing period is part of the word, not a sentence
/// end (cotabby SentenceBoundaryClassifier).
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "st", "vs", "eg", "ie", "etc", "no", "fig", "approx", "inc", "ltd",
];

fn is_closing_punct(c: char) -> bool {
    matches!(c, '"' | '\'' | ')' | ']' | '}' | '\u{201D}' | '\u{2019}')
}

/// Whether the period ending at byte index `period_idx` terminates a sentence.
/// Decimals/list numbers ("3.14", "1."), single-letter initials ("U.", the "S." in
/// "U.S."), and known abbreviations ("e.g.", "etc.") do not.
fn is_terminal_period(text: &str, period_idx: usize) -> bool {
    let before = &text[..period_idx];
    let Some(prev) = before.chars().last() else {
        return true;
    };
    if prev.is_numeric() {
        return false;
    }
    if prev.is_alphabetic() {
        let word: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphabetic())
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        if word.chars().count() == 1 {
            return false;
        }
        if ABBREVIATIONS.contains(&word.to_lowercase().as_str()) {
            return false;
        }
    }
    true
}

/// Whether `text` ends at a real sentence boundary: after skipping trailing whitespace
/// and a run of closing quotes/brackets, the last character is `!`, `?`, or a terminal
/// period (cotabby SentenceBoundaryClassifier.endsSentence).
fn ends_sentence(text: &str) -> bool {
    let mut s = text.trim_end();
    while let Some(c) = s.chars().last() {
        if is_closing_punct(c) {
            s = &s[..s.len() - c.len_utf8()];
        } else {
            break;
        }
    }
    match s.chars().last() {
        Some('!') | Some('?') => true,
        Some('.') => is_terminal_period(s, s.len() - 1),
        _ => false,
    }
}

pub fn truncate_at_sentence_boundary(mut text: String) -> String {
    let mut end: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match c {
            '\n' => {
                end = Some(i);
                break;
            }
            '!' | '?' => {
                end = Some(i + c.len_utf8());
                break;
            }
            '.' if is_terminal_period(&text, i) => {
                end = Some(i + 1);
                break;
            }
            _ => {}
        }
    }
    if let Some(mut e) = end {
        // Keep closing quotes/brackets attached to the terminator ("done.")…).
        while let Some(c) = text[e..].chars().next() {
            if is_closing_punct(c) {
                e += c.len_utf8();
            } else {
                break;
            }
        }
        text.truncate(e);
    }
    text.trim_end().to_string()
}

/// Multi-line variant: allow single newlines but stop at the first blank line (`\n\n`).
pub fn truncate_at_blank_line(mut text: String) -> String {
    if let Some(pos) = text.find("\n\n") {
        text.truncate(pos);
    }
    // Still truncate at the first real sentence boundary before any newline.
    let first_nl = text.find('\n').unwrap_or(text.len());
    let mut end: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if i >= first_nl {
            break;
        }
        match c {
            '!' | '?' => {
                end = Some(i + c.len_utf8());
                break;
            }
            '.' if is_terminal_period(&text, i) => {
                end = Some(i + 1);
                break;
            }
            _ => {}
        }
    }
    if let Some(e) = end {
        text.truncate(e);
    }
    text.trim_end().to_string()
}

/// True when `completion` would mostly retype text that already follows the caret
/// (cotabby TrailingDuplicationFilter). Comparison runs on a folded view — lowercase,
/// alphanumerics only — so a stray leading bullet, quote, or case difference cannot
/// defeat the match.
pub fn duplicates_trailing_text(completion: &str, trailing: &str) -> bool {
    const MIN_FOLDED_OVERLAP: usize = 3;
    fn fold(s: &str) -> Vec<char> {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    }
    let fc = fold(completion);
    if fc.len() < MIN_FOLDED_OVERLAP {
        return false;
    }
    let ft = fold(trailing);
    if ft.is_empty() {
        return false;
    }
    // Shape 1: the completion is the start of what already follows the caret.
    if ft.len() >= fc.len() && ft[..fc.len()] == fc[..] {
        return true;
    }
    // Shape 2: the completion contains the whole upcoming suffix run.
    if ft.len() >= MIN_FOLDED_OVERLAP && fc.len() >= ft.len() && fc[..ft.len()] == ft[..] {
        return true;
    }
    // Shape 3: a long leading run of the completion already appears at the caret.
    let overlap = fc.iter().zip(ft.iter()).take_while(|(a, b)| a == b).count();
    overlap >= MIN_FOLDED_OVERLAP.max(fc.len() / 2)
}

// ── Completion normaliser ─────────────────────────────────────────────────────

/// Cleans raw model output before it is surfaced to the user.
///
/// Passes in order:
/// 1. Strip chat-control tokens and `<think>` blocks (including unclosed).
/// 2. Collapse `\r`.
/// 3. Echo suppression — strip the longest word-suffix of `prefix` that
///    matches the start of the completion.  If that suffix spans the entire
///    last sentence fragment of the prefix, the completion is suppressed
///    entirely (returns `""`), because the model restarted from the beginning
///    of the user's thought instead of continuing after it.
/// 4. Leading-whitespace normalisation — if `prefix` ends with whitespace,
///    strip any leading whitespace from the result to prevent double-spacing.
pub fn normalize_completion(raw: String, prefix: &str) -> String {
    let text = strip_think_blocks(&raw);
    let mut text = text
        .replace("<|im_start|>assistant", "")
        .replace("<|im_start|>", "")
        .replace("<|im_end|>", "")
        .replace("<start_of_turn>model", "")
        .replace("<start_of_turn>", "")
        .replace("<end_of_turn>", "")
        .replace("<|turn>model", "")
        .replace("<|turn>", "")
        .replace("<turn|>", "");

    text = text.replace('\r', "");
    text = suppress_echo(text, prefix);

    if prefix.ends_with(|c: char| c.is_whitespace()) {
        text = text.trim_start().to_string();
    }

    text
}

fn strip_think_blocks(text: &str) -> String {
    let mut result = text.to_string();
    loop {
        match result.find("<think>") {
            None => break,
            Some(start) => match result[start..].find("</think>") {
                Some(rel_end) => {
                    result.replace_range(start..start + rel_end + "</think>".len(), "");
                }
                None => {
                    result.truncate(start);
                    break;
                }
            },
        }
    }
    result
}

/// Strip the longest word-level suffix of `prefix` that appears at the start
/// of `completion`.  If the match covers the entire last sentence fragment of
/// the prefix (up to 15 words), the completion is fully suppressed.
fn suppress_echo(completion: String, prefix: &str) -> String {
    let fragment = prefix
        .rsplit(|c: char| matches!(c, '\n' | '.' | '!' | '?'))
        .next()
        .unwrap_or(prefix);

    let all_fragment_words: Vec<&str> = fragment.split_whitespace().collect();
    if all_fragment_words.is_empty() {
        return completion;
    }
    let cap = all_fragment_words.len().min(15);
    let fragment_words = &all_fragment_words[all_fragment_words.len() - cap..];

    // Build (byte_start, byte_end) spans for each word in the completion.
    let mut comp_spans: Vec<(usize, usize)> = Vec::new();
    let mut word_start: Option<usize> = None;
    for (i, c) in completion.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = word_start.take() {
                comp_spans.push((s, i));
            }
        } else if word_start.is_none() {
            word_start = Some(i);
        }
    }
    if let Some(s) = word_start {
        comp_spans.push((s, completion.len()));
    }

    if comp_spans.is_empty() {
        return completion;
    }

    // Try the longest suffix first (greedy).
    for n in (1..=fragment_words.len()).rev() {
        if comp_spans.len() < n {
            continue;
        }
        let suffix = &fragment_words[fragment_words.len() - n..];
        let all_match = suffix
            .iter()
            .zip(comp_spans[..n].iter())
            .all(|(fw, &(s, e))| fw.eq_ignore_ascii_case(&completion[s..e]));

        if all_match {
            if n == fragment_words.len() {
                return String::new();
            }
            let (_, end) = comp_spans[n - 1];
            return completion[end..].to_string();
        }
    }

    completion
}

/// Lowercased alphanumeric word list, splitting on any non-alphanumeric char.
fn words_lower(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Drop a completion that parrots the captured background context (on-screen OCR or
/// clipboard) instead of continuing the user's text. Base/continuation models will
/// sometimes copy a chunk of the injected "Nearby on screen: …" preface verbatim.
/// Any contiguous run of `MIN_SHARED_WORDS` words shared verbatim (case-insensitive)
/// between the completion and the context is treated as parroting, and the whole
/// completion is suppressed. Shorter overlaps (incidental stop-word phrases) are left
/// alone so genuine continuations that happen to share a few common words survive.
fn suppress_context_copy(completion: String, ctx: &InstrContext) -> String {
    const MIN_SHARED_WORDS: usize = 4;

    let comp_words = words_lower(&completion);
    if comp_words.len() < MIN_SHARED_WORDS {
        return completion;
    }

    let context = format!("{} {}", ctx.visual_context, ctx.clipboard_context);
    let ctx_words = words_lower(&context);
    if ctx_words.len() < MIN_SHARED_WORDS {
        return completion;
    }

    let parrots = comp_words
        .windows(MIN_SHARED_WORDS)
        .any(|window| ctx_words.windows(MIN_SHARED_WORDS).any(|w| w == window));

    if parrots {
        String::new()
    } else {
        completion
    }
}

// ── Stub completer for tests ──────────────────────────────────────────────────

#[cfg(test)]
pub struct StubCompleter {
    pub response: String,
}

#[cfg(test)]
impl Completer for StubCompleter {
    fn complete_ext(&self, _prefix: &str, _suffix: &str, _max_tokens: u32, _multi_line: bool) -> Result<String> {
        Ok(self.response.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sentence-boundary classifier ─────────────────────────────────────────

    #[test]
    fn ends_sentence_plain_terminators() {
        assert!(ends_sentence("That is done."));
        assert!(ends_sentence("Really?"));
        assert!(ends_sentence("Stop!"));
        assert!(!ends_sentence("not finished yet"));
    }

    #[test]
    fn ends_sentence_skips_decimals_and_abbreviations() {
        assert!(!ends_sentence("pi is 3."));
        assert!(!ends_sentence("see e.g."));
        assert!(!ends_sentence("ask Dr."));
        assert!(!ends_sentence("the U."));
    }

    #[test]
    fn ends_sentence_walks_back_closing_punctuation() {
        assert!(ends_sentence("he said \"stop.\""));
        assert!(ends_sentence("(done!)"));
    }

    #[test]
    fn truncate_keeps_decimals_intact() {
        let s = "costs 3.50 dollars today".to_string();
        assert_eq!(truncate_at_sentence_boundary(s), "costs 3.50 dollars today");
    }

    #[test]
    fn truncate_keeps_abbreviation_and_stops_at_real_end() {
        let s = "e.g. apples are good. And more".to_string();
        assert_eq!(truncate_at_sentence_boundary(s), "e.g. apples are good.");
    }

    #[test]
    fn truncate_keeps_closing_quote_with_terminator() {
        let s = "she said \"go.\" Then left".to_string();
        assert_eq!(truncate_at_sentence_boundary(s), "she said \"go.\"");
    }

    // ── trailing-duplication filter ───────────────────────────────────────────

    #[test]
    fn trailing_dup_detects_prefix_of_suffix() {
        assert!(duplicates_trailing_text("world", " world and more"));
    }

    #[test]
    fn trailing_dup_ignores_case_and_stray_glyphs() {
        assert!(duplicates_trailing_text("- World", "world and more"));
    }

    #[test]
    fn trailing_dup_detects_completion_containing_suffix() {
        assert!(duplicates_trailing_text("the end of it", "the end"));
    }

    #[test]
    fn trailing_dup_allows_fresh_text() {
        assert!(!duplicates_trailing_text("something new", "completely different"));
    }

    #[test]
    fn trailing_dup_ignores_short_coincidences() {
        assert!(!duplicates_trailing_text("th", "the rest"));
    }

    // ── prefix windowing ──────────────────────────────────────────────────────

    #[test]
    fn window_keeps_short_prefix_whole() {
        assert_eq!(window_prefix("hello brave world"), "hello brave world");
    }

    #[test]
    fn window_trims_trailing_whitespace() {
        assert_eq!(window_prefix("hello world "), "hello world");
    }

    #[test]
    fn window_caps_word_count() {
        let long: String = (0..80).map(|i| format!("w{i} ")).collect();
        let windowed = window_prefix(&long);
        assert_eq!(windowed.split_whitespace().count(), 50);
        assert!(windowed.ends_with("w79"));
    }

    // ── base-model conditioning preface ───────────────────────────────────────

    #[test]
    fn preface_empty_without_context() {
        assert_eq!(base_preface(&InstrContext::default()), "");
    }

    #[test]
    fn preface_conditions_rather_than_commands() {
        let ctx = InstrContext {
            user_name: "Mubarek".into(),
            custom_rules: "avoid passive voice\nPersonal vocabulary: tabtypist, ghazal".into(),
            language: "Amharic".into(),
            ..Default::default()
        };
        let p = base_preface(&ctx);
        assert!(p.contains("Written by Mubarek."));
        assert!(p.contains("Writing style: avoid passive voice."));
        assert!(p.contains("Often uses the words: tabtypist, ghazal."));
        assert!(p.contains("Written in Amharic."));
        // App name must never appear (biases base models toward code/numbers).
        assert!(!p.to_lowercase().contains("app"));
    }

    #[test]
    fn preface_keeps_screen_tail_flattened() {
        let ctx = InstrContext {
            visual_context: "far away line\nnearest the field".into(),
            ..Default::default()
        };
        let p = base_preface(&ctx);
        assert!(p.contains("Nearby on screen: far away line nearest the field"));
    }

    // ── context-copy suppression ──────────────────────────────────────────────

    #[test]
    fn suppresses_completion_copied_from_screen() {
        let ctx = InstrContext {
            visual_context: "The quarterly report is due on Friday afternoon".into(),
            ..Default::default()
        };
        // Model parroted a verbatim run of the on-screen text.
        let out = suppress_context_copy("report is due on Friday".into(), &ctx);
        assert_eq!(out, "");
    }

    #[test]
    fn keeps_genuine_continuation() {
        let ctx = InstrContext {
            visual_context: "The quarterly report is due on Friday afternoon".into(),
            ..Default::default()
        };
        // Shares only the common word "the" — not a contiguous 4-word run.
        let out = suppress_context_copy("and the team will review it next week".into(), &ctx);
        assert_eq!(out, "and the team will review it next week");
    }

    #[test]
    fn short_completion_never_suppressed() {
        let ctx = InstrContext {
            visual_context: "please send the invoice".into(),
            ..Default::default()
        };
        // Under the 4-word minimum, so left alone even though it overlaps.
        let out = suppress_context_copy("send the".into(), &ctx);
        assert_eq!(out, "send the");
    }

    #[test]
    fn suppresses_copy_from_clipboard() {
        let ctx = InstrContext {
            clipboard_context: "meeting moved to three o'clock tomorrow".into(),
            ..Default::default()
        };
        let out = suppress_context_copy("moved to three o'clock".into(), &ctx);
        assert_eq!(out, "");
    }

    // ── common token prefix ───────────────────────────────────────────────────

    #[test]
    fn common_prefix_counts_shared_lead() {
        let a = [LlamaToken(1), LlamaToken(2), LlamaToken(3)];
        let b = [LlamaToken(1), LlamaToken(2), LlamaToken(9)];
        assert_eq!(common_prefix_len(&a, &b), 2);
        assert_eq!(common_prefix_len(&a, &[]), 0);
    }

    #[test]
    fn stutter_strips_repeated_connector() {
        assert_eq!(
            strip_trailing_word_stutter("I went to the store and and"),
            Some("I went to the store and".to_string())
        );
    }

    #[test]
    fn stutter_case_insensitive() {
        assert_eq!(
            strip_trailing_word_stutter("Wait The the"),
            Some("Wait The".to_string())
        );
    }

    #[test]
    fn stutter_ignores_non_duplicates() {
        assert_eq!(strip_trailing_word_stutter("a clean continuation here"), None);
        // Different words that merely share a prefix are not a stutter.
        assert_eq!(strip_trailing_word_stutter("and android"), None);
    }

    #[test]
    fn truncate_at_period() {
        let s = "Hello world. And more text here.".to_string();
        assert_eq!(truncate_at_sentence_boundary(s), "Hello world.");
    }

    #[test]
    fn truncate_at_newline() {
        let s = "First line\nSecond line".to_string();
        assert_eq!(truncate_at_sentence_boundary(s), "First line");
    }

    #[test]
    fn truncate_no_boundary() {
        let s = "no sentence end here".to_string();
        assert_eq!(truncate_at_sentence_boundary(s), "no sentence end here");
    }

    // ── truncate_at_blank_line ────────────────────────────────────────────────

    #[test]
    fn blank_line_truncates_at_double_newline() {
        let s = "line one\nline two\n\nline three".to_string();
        assert_eq!(truncate_at_blank_line(s), "line one\nline two");
    }

    #[test]
    fn blank_line_single_newline_passes_through() {
        let s = "line one\nline two".to_string();
        assert_eq!(truncate_at_blank_line(s), "line one\nline two");
    }

    // ── normalize_completion ──────────────────────────────────────────────────

    #[test]
    fn strips_im_end_token() {
        let out = normalize_completion("great idea<|im_end|>".into(), "that is a ");
        assert_eq!(out, "great idea");
    }

    #[test]
    fn strips_im_start_tokens() {
        let out = normalize_completion("<|im_start|>assistant hello".into(), "say");
        assert_eq!(out, " hello");
    }

    #[test]
    fn strips_complete_think_block() {
        let out = normalize_completion("<think>reasoning here</think>actual answer".into(), "q: ");
        assert_eq!(out, "actual answer");
    }

    #[test]
    fn strips_unclosed_think_tag() {
        let out = normalize_completion("<think>started but never ended".into(), "q: ");
        assert_eq!(out, "");
    }

    #[test]
    fn collapses_carriage_return() {
        let out = normalize_completion("line one\r\nline two".into(), "start ");
        assert_eq!(out, "line one\nline two");
    }

    #[test]
    fn echo_suppression_partial() {
        // Completion starts with the last word of prefix — strip that word.
        let out = normalize_completion("world is great".into(), "hello world");
        assert_eq!(out, " is great");
    }

    #[test]
    fn echo_suppression_full_fragment() {
        // Completion starts with the ENTIRE last fragment of prefix — suppress.
        let out = normalize_completion("I like to eat".into(), "I like");
        assert_eq!(out, "");
    }

    #[test]
    fn echo_suppression_case_insensitive() {
        let out = normalize_completion("World is great".into(), "hello world");
        assert_eq!(out, " is great");
    }

    #[test]
    fn echo_suppression_no_match() {
        let out = normalize_completion("something new".into(), "hello world");
        assert_eq!(out, "something new");
    }

    #[test]
    fn leading_whitespace_stripped_when_prefix_ends_in_space() {
        let out = normalize_completion(" great idea".into(), "that is ");
        assert_eq!(out, "great idea");
    }

    #[test]
    fn leading_whitespace_preserved_when_prefix_ends_in_word_char() {
        let out = normalize_completion(" great".into(), "hello");
        assert_eq!(out, " great");
    }
}
