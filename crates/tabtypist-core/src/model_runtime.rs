use anyhow::{Context, Result};
use encoding_rs;
use llama_cpp_2::token::LlamaToken;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

/// A loaded model that can produce completions.
pub trait Completer: Send + Sync {
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    seed_penalty_window(&mut sampler, &new_tokens);

    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut result = String::new();
    let mut tokens_emitted = 0usize;

    let mut token = sampler.sample(ctx, sample_idx);
    sampler.accept(token);
    let mut argmax_eog = argmax_is_eog(model, ctx, sample_idx);

    for _ in 0..max_tokens {
        // Cooperative cancellation: a newer keystroke superseded this request, so
        // stop decoding and free the inference thread for it. The KV trim below
        // still runs, leaving the cache valid for the next (reusing) request.
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // The model's most-likely token is end-of-generation even though the
        // sampler drew something else: finalize with the text so far and discard
        // the sampled-but-unwanted token.
        if argmax_eog {
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
            // Word-stutter stop ("and and", "the the"): the model started looping.
            // Same guard as the instruct path — drop the duplicate and stop.
            if let Some(trimmed) = strip_trailing_word_stutter(&result) {
                result = trimmed;
                break;
            }
            // Phrase-loop stop ("I'm gonna die I'm gonna die"): the model is
            // cycling a multi-word phrase the stutter guard can't see. Keep one
            // occurrence and stop.
            if let Some(trimmed) = strip_trailing_phrase_loop(&result) {
                result = trimmed;
                break;
            }
            // Scaffolding stop: the normaliser truncates at the first stop marker
            // anyway, so everything past it is guaranteed-discarded work. No
            // min-token guard — a marker means the model believes the turn is over.
            if contains_scaffolding_marker(&result) {
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
        ctx.decode(&mut next).context("autoregressive decode")?;
        pos += 1;

        token = sampler.sample(ctx, 0);
        sampler.accept(token);
        argmax_eog = argmax_is_eog(model, ctx, 0);
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
    seed_penalty_window(&mut sampler, &tokens);

    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut result = String::new();
    let mut tokens_emitted = 0usize;
    let mut pos = tokens.len() as i32;

    // Confidence tracking — cotabby's text-stream gate (LlamaGenerationOptions.confidenceFloor).
    // Accumulate the average per-token log-probability of the emitted tokens. When the model
    // runs past a natural stopping point and keeps the sentence going by inventing/chaining,
    // its per-token confidence falls; a completion whose average drops below the floor is
    // suppressed wholesale rather than shown as a run-on. Tunable via TABTYPIST_CONFIDENCE_FLOOR.
    //
    // The floor is read up front because the per-token log-softmax it feeds costs a full
    // exp() pass over the vocabulary; with the floor at its default (-inf, disabled) and
    // the CONFIDENCE log off, nothing consumes the value, so the pass is skipped entirely
    // (cotabby's "gated logprobs"). The argmax-EOG check shares the remaining single scan.
    let floor = std::env::var("TABTYPIST_CONFIDENCE_FLOOR")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(DEFAULT_CONFIDENCE_FLOOR);
    let log_confidence = std::env::var("TABTYPIST_LOG_PROMPT").is_ok();
    let compute_lp = floor > f64::NEG_INFINITY || log_confidence;
    let mut sum_lp = 0f64;
    let mut n_lp = 0usize;

    let mut token = sampler.sample(ctx, last_idx as i32);
    sampler.accept(token);
    let (argmax, lp) = scan_logits(ctx, last_idx as i32, compute_lp.then_some(token));
    let mut argmax_eog = model.is_eog_token(argmax);
    let mut cur_lp = lp;

    for _ in 0..max_tokens {
        // Cooperative cancellation — see do_complete. The KV trim below still runs.
        if cancel.load(Ordering::Relaxed) { break; }
        // Most-likely token is end-of-generation — see do_complete.
        if argmax_eog { break; }
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
            // Phrase-loop stop — see do_complete.
            if let Some(trimmed) = strip_trailing_phrase_loop(&result) {
                result = trimmed;
                break;
            }
            // Scaffolding stop — see do_complete. Catches markers that tokenize
            // into multiple pieces, which the single-token stop_tokens check misses.
            if contains_scaffolding_marker(&result) {
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
        sampler.accept(token);
        let (argmax, lp) = scan_logits(ctx, 0, compute_lp.then_some(token));
        argmax_eog = model.is_eog_token(argmax);
        cur_lp = lp;
    }

    // Trim generated tokens out of the KV cache so the next call's common-prefix
    // check (against kv_tokens, prompt-only) matches the cache's real contents.
    let _ = ctx.clear_kv_cache_seq(Some(0), Some(kv_tokens.len() as u32), None);

    // Confidence floor: suppress a low-confidence (rambling/invented) completion entirely.
    let avg_lp = if n_lp > 0 { sum_lp / n_lp as f64 } else { 0.0 };
    if log_confidence {
        tracing::info!(
            "CONFIDENCE: avg_logprob={:.3} over {} tokens (floor={:.3}) raw={:?}",
            avg_lp, n_lp, floor, result
        );
    }
    if compute_lp && n_lp > 0 && avg_lp < floor {
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

/// If `text` ends with the same multi-word phrase twice in a row ("I'm gonna die
/// I'm gonna die"), return it cut back to a single occurrence; otherwise `None`.
/// The phrase-level sibling of [`strip_trailing_word_stutter`]: a looping model
/// cycles whole phrases at least as often as single words, and nothing else in the
/// chain can see it — the repeat penalty (1.05) provably fails to stop it, and the
/// echo/duplication filters compare against the USER's text, not the completion's
/// own tail. (Neither cotabby nor cotypist guards this shape.)
///
/// Words compare case-insensitively but otherwise verbatim, so punctuation drift
/// protects legitimate repeats: "New York, New York" survives because "York," and
/// "York" differ. Running per decoded token means a loop is cut the moment its
/// second occurrence completes, so longer runs never reach the user.
fn strip_trailing_phrase_loop(text: &str) -> Option<String> {
    const MIN_PHRASE_WORDS: usize = 2; // single words are the stutter guard's job
    const MAX_PHRASE_WORDS: usize = 8;

    // Byte spans of each whitespace-separated word, so the cut lands exactly at
    // the second occurrence's first byte.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                spans.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        spans.push((s, text.len()));
    }

    let n = spans.len();
    let word = |i: usize| &text[spans[i].0..spans[i].1];
    for k in MIN_PHRASE_WORDS..=MAX_PHRASE_WORDS {
        if n < 2 * k {
            break;
        }
        let repeats = (0..k).all(|j| word(n - k + j).eq_ignore_ascii_case(word(n - 2 * k + j)));
        if !repeats {
            continue;
        }
        // A repeating run with no letters ("2 3 2 3") is a list or score line,
        // not a language loop — leave numeric patterns alone.
        if !(n - k..n).any(|i| word(i).chars().any(|c| c.is_alphabetic())) {
            continue;
        }
        return Some(text[..spans[n - k].0].trim_end().to_string());
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

/// One fused scan of the logits at output position `idx`, so the decode loop pays for
/// exactly what it consumes (cotabby's "gated logprobs" perf fix does the same):
/// - always returned: the argmax token (for the argmax-EOG stop) — one comparison
///   pass, no `exp()` work;
/// - when `logprob_token` is set: the log-probability the model assigned to that
///   token, via a numerically-stable log-softmax (`logit[t] - logsumexp`). This adds
///   a full `exp()` pass over the vocabulary (~150K calls), which is why it must stay
///   off unless the confidence floor or its debug log actually reads the value.
fn scan_logits(
    ctx: &llama_cpp_2::context::LlamaContext,
    idx: i32,
    logprob_token: Option<LlamaToken>,
) -> (LlamaToken, f32) {
    let logits = ctx.get_logits_ith(idx);
    let mut best = 0usize;
    let mut max = f32::NEG_INFINITY;
    for (i, &v) in logits.iter().enumerate() {
        if v > max {
            max = v;
            best = i;
        }
    }
    let argmax = LlamaToken(best as i32);
    let Some(token) = logprob_token else {
        return (argmax, 0.0);
    };
    let id = token.0 as usize;
    if id >= logits.len() {
        return (argmax, 0.0);
    }
    let mut sumexp = 0f32;
    for &l in logits {
        sumexp += (l - max).exp();
    }
    (argmax, logits[id] - (max + sumexp.ln()))
}

/// True when the raw distribution's most-likely token at output position `idx` is an
/// end-of-generation token: the model wants to stop here even though the stochastic
/// sampler drew something else (cotabby LlamaGenerationOptions.stopAtArgmaxEOG, ON by
/// default there too). This is the anti-rambling stop the sentence classifier cannot
/// express — lists, fragments, code — and it fires BEFORE the sampled-but-unwanted
/// token is appended, so the completion finalises with the text accumulated so far.
fn argmax_is_eog(
    model: &llama_cpp_2::model::LlamaModel,
    ctx: &llama_cpp_2::context::LlamaContext,
    idx: i32,
) -> bool {
    model.is_eog_token(scan_logits(ctx, idx, None).0)
}

/// True when the completion introduces a run of four or more identical
/// punctuation/symbol characters ("....", "$$$$") — decode noise, never prose
/// (cotabby CompletionSeamGuard's junk-run rule). A run flush against the caret that
/// continues an identical run the user already has is an existing divider being
/// extended, not fresh junk — but the preceding run must be a real one (2+): a
/// sentence that merely ends in "." must not exempt "....".
pub fn introduces_junk_punctuation_run(completion: &str, prefix: &str) -> bool {
    const JUNK_RUN_LEN: usize = 4;
    let mut run_char: Option<char> = None;
    let mut run_len = 0usize;
    let mut run_starts_at_start = false;
    for (i, c) in completion.chars().enumerate() {
        if Some(c) == run_char {
            run_len += 1;
        } else {
            run_char = Some(c);
            run_len = 1;
            run_starts_at_start = i == 0;
        }
        if run_len < JUNK_RUN_LEN || c.is_alphanumeric() || c.is_whitespace() {
            continue;
        }
        if run_starts_at_start && prefix.chars().rev().take_while(|&p| p == c).count() >= 2 {
            continue;
        }
        return true;
    }
    false
}

/// Minimum tokens generated before the sentence-boundary early stop may fire, guarding
/// against degenerate instant stops like a lone leading period (cotabby DecodeStopPolicy).
const SENTENCE_STOP_MIN_TOKENS: usize = 2;

/// Tokens the repeat-penalty stage remembers (its `penalty_last_n` ring buffer).
const PENALTY_WINDOW: usize = 64;

/// Shared sampler chain for both inference paths — cotabby's shipped SamplingConfig:
/// gentle repeat penalty (1.05; heavier values distort word choice mid-sentence),
/// top-k 20 → top-p 0.7 → min-p 0.08 → temp 0.1, then dist with a FIXED seed so the
/// same context always produces the same ghost text (their defaultSamplerSeed).
fn completion_sampler() -> llama_cpp_2::sampling::LlamaSampler {
    use llama_cpp_2::sampling::LlamaSampler;
    LlamaSampler::chain_simple([
        LlamaSampler::penalties(PENALTY_WINDOW as i32, 1.05, 0.0, 0.0),
        LlamaSampler::top_k(20),
        LlamaSampler::top_p(0.7, 1),
        LlamaSampler::min_p(0.08, 1),
        LlamaSampler::temp(0.1),
        LlamaSampler::dist(0x00C0_FFEE),
    ])
}

/// Feed the prompt tail into the sampler's repeat-penalty ring buffer. The penalties
/// stage only sees tokens passed through `accept()`, so without this it starts every
/// generation blind to the text the user just wrote and will cheerfully re-emit the
/// sentence they just finished (or just accepted). Seeding the last PENALTY_WINDOW
/// prompt tokens makes a verbatim repeat pay the penalty from token one.
fn seed_penalty_window(
    sampler: &mut llama_cpp_2::sampling::LlamaSampler,
    prompt_tokens: &[LlamaToken],
) {
    let start = prompt_tokens.len().saturating_sub(PENALTY_WINDOW);
    for &tok in &prompt_tokens[start..] {
        sampler.accept(tok);
    }
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
pub fn ends_sentence(text: &str) -> bool {
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

/// True when the completion contains a clock time ("7:00", "18:45") while the user's
/// own text contains none. A generated timestamp the user never set up is the model
/// imitating chat-transcript chrome from the screen context — timestamps between
/// messages teach it that "sentence, time, sentence" is the local format, and the
/// output stops being a continuation of the user's words ("…for a long time 7:00 in
/// the evening I'm not sure…"). Times the user is already writing about pass through.
pub fn mimics_transcript_format(completion: &str, prefix: &str) -> bool {
    contains_clock_time(completion) && !contains_clock_time(prefix)
}

fn contains_clock_time(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    for i in 1..c.len() {
        if c[i] == ':'
            && c[i - 1].is_ascii_digit()
            && i + 2 < c.len()
            && c[i + 1].is_ascii_digit()
            && c[i + 2].is_ascii_digit()
        {
            return true;
        }
    }
    false
}

/// Lowercased, alphanumerics-only view of `s`, used for duplication checks so case,
/// spacing, and punctuation drift cannot defeat a match.
pub fn fold_alnum(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// True when `completion` retypes text the user already wrote just before the caret —
/// the base-model loop of re-emitting the sentence that was just finished (typically
/// the suggestion the user just accepted). Folded comparison; short completions are
/// exempt because brief common phrases legitimately recur in normal writing.
pub fn duplicates_preceding_text(completion: &str, prefix: &str) -> bool {
    const MIN_FOLDED_LEN: usize = 10;
    const PREFIX_TAIL_CHARS: usize = 600;

    let fc = fold_alnum(completion);
    if fc.chars().count() < MIN_FOLDED_LEN {
        return false;
    }
    let tail: String = {
        let count = prefix.chars().count();
        prefix
            .chars()
            .skip(count.saturating_sub(PREFIX_TAIL_CHARS))
            .collect()
    };
    fold_alnum(&tail).contains(&fc)
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

/// Opening / role markers (cotabby ControlTokenMarkers.openingMarkers plus the
/// gemma-4 `<|turn>` family). The real continuation sits ADJACENT to these, so only
/// the marker itself is removed and the surrounding text kept. Longer variants must
/// precede their prefixes ("<|im_start|>assistant" before "<|im_start|>") or the
/// role word would survive the strip.
const OPENING_MARKERS: &[&str] = &[
    "<|im_start|>assistant",
    "<|im_start|>",
    "<start_of_turn>model",
    "<start_of_turn>",
    "<|turn>model",
    "<|turn>",
    "<|user|>",
    "<|assistant|>",
    "<|system|>",
    "<|start_header_id|>",
    "<|end_header_id|>",
    "[INST]",
    "[/INST]",
];

/// Stop / end-of-turn markers (cotabby ControlTokenMarkers.stopMarkers plus gemma-4
/// `<turn|>`). A stop marker means the model believes the turn is over: everything
/// after it is a hallucinated NEW turn, never a continuation of the user's text, so
/// the completion is truncated at the first one — removing the marker in place would
/// splice that next turn onto the real completion. `</s>` is deliberately absent: it
/// is also the closing tag of HTML's `<s>` element, and truncating on it would cut
/// HTML authoring.
const STOP_MARKERS: &[&str] = &[
    "<|im_end|>",
    "<|endoftext|>",
    "<|end|>",
    "<end_of_turn>",
    "<|eot_id|>",
    "<turn|>",
];

/// Decode-time scaffolding stop (cotabby DecodeStopPolicy.scaffoldingMarker): once a
/// stop marker is in the accumulated text, every further token is guaranteed-discarded
/// work — the normaliser truncates there anyway — so the decode loop stops immediately,
/// exactly in the worst case where the model has drifted into template scaffolding.
/// The `<` pre-filter keeps ordinary prose from ever reaching the per-marker scan.
fn contains_scaffolding_marker(text: &str) -> bool {
    text.contains('<') && STOP_MARKERS.iter().any(|m| text.contains(m))
}

/// Cleans raw model output before it is surfaced to the user.
///
/// Passes in order:
/// 1. Strip `<think>` blocks (including unclosed) and opening/role chat markers;
///    truncate at the first stop/end-of-turn marker (text past it is a new turn).
/// 2. Collapse `\r`.
/// 3. Echo suppression — strip the longest word-suffix of `prefix` that
///    matches the start of the completion.  If that suffix spans the entire
///    last sentence fragment of the prefix, the completion is suppressed
///    entirely (returns `""`), because the model restarted from the beginning
///    of the user's thought instead of continuing after it.
/// 4. Leading-whitespace normalisation — if `prefix` ends with whitespace,
///    strip any leading whitespace from the result to prevent double-spacing.
pub fn normalize_completion(raw: String, prefix: &str) -> String {
    let mut text = strip_think_blocks(&raw);
    for marker in OPENING_MARKERS {
        if text.contains(marker) {
            text = text.replace(marker, "");
        }
    }
    if let Some(cut) = STOP_MARKERS.iter().filter_map(|m| text.find(m)).min() {
        text.truncate(cut);
    }

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

/// Common English words excluded from the paraphrase-overlap measure below: replies
/// legitimately reuse these from the message they answer, so counting them would
/// suppress ordinary responses.
const COMMON_WORDS: &[&str] = &[
    "that", "this", "with", "would", "could", "should", "have", "will", "your",
    "from", "they", "them", "there", "their", "then", "than", "what", "when",
    "where", "which", "been", "were", "some", "more", "only", "also", "very",
    "just", "like", "about", "into", "over", "because", "really", "want", "make",
    "know", "think", "going", "good", "well", "much", "still", "even", "here",
];

/// Drop a completion that parrots the captured background context (on-screen OCR or
/// clipboard) instead of continuing the user's text. Two shapes are caught:
///
/// 1. Verbatim copy — any contiguous run of `MIN_SHARED_WORDS` words shared
///    (case-insensitive) between the completion and the context.
/// 2. Paraphrased regurgitation — nearly all of the completion's distinctive content
///    words (4+ chars, excluding [`COMMON_WORDS`]) already appear somewhere in the
///    context. A real continuation of the *user's* thought brings its own vocabulary;
///    one assembled almost entirely from on-screen words is the context talking, not
///    the user, and reads as incoherent with the sentence in hand.
fn suppress_context_copy(completion: String, ctx: &InstrContext) -> String {
    const MIN_SHARED_WORDS: usize = 4;
    const MIN_CONTENT_WORDS: usize = 3;
    const MAX_CONTENT_OVERLAP: f64 = 0.75;

    let comp_words = words_lower(&completion);
    let context = format!("{} {}", ctx.visual_context, ctx.clipboard_context);
    let ctx_words = words_lower(&context);
    if ctx_words.len() < MIN_SHARED_WORDS {
        return completion;
    }

    // A shared run must contain at least one distinctive word: four common words in
    // a row ("i think that would") recur in any two texts on the same topic and are
    // not evidence of copying.
    let distinctive =
        |w: &String| w.chars().count() >= 4 && !COMMON_WORDS.contains(&w.as_str());
    let parrots = comp_words.len() >= MIN_SHARED_WORDS
        && comp_words.windows(MIN_SHARED_WORDS).any(|window| {
            window.iter().any(distinctive)
                && ctx_words.windows(MIN_SHARED_WORDS).any(|w| w == window)
        });

    let content: Vec<&str> = comp_words
        .iter()
        .map(String::as_str)
        .filter(|w| w.chars().count() >= 4 && !COMMON_WORDS.contains(w))
        .collect();
    let paraphrases = content.len() >= MIN_CONTENT_WORDS && {
        let ctx_set: std::collections::HashSet<&str> =
            ctx_words.iter().map(String::as_str).collect();
        let shared = content.iter().filter(|w| ctx_set.contains(**w)).count();
        shared as f64 / content.len() as f64 >= MAX_CONTENT_OVERLAP
    };

    if parrots || paraphrases {
        String::new()
    } else {
        completion
    }
}

/// Final safety predicate before a completion may be shown or inserted (cotabby
/// InsertionSafetyGate, original implementation): rejects U+FFFD (lossy
/// detokenization), control characters other than newline (corruption, never
/// content), and whitespace-only output. Deliberately does NOT judge punctuation —
/// a lone ")" or "." is a legitimate inline completion.
pub fn is_safe_to_insert(completion: &str) -> bool {
    let mut saw_content = false;
    for c in completion.chars() {
        if c == '\u{FFFD}' {
            return false;
        }
        if c != '\n' && c.is_control() {
            return false;
        }
        if !c.is_whitespace() {
            saw_content = true;
        }
    }
    saw_content
}

/// True when the prefix ends mid-word (a lowercase letter) and the completion
/// immediately opens a NEW capitalized word with no separator. A coherent
/// continuation of "…not seeing a simila" finishes the word ("r view…"); starting
/// "This war is going to…" right against it means the model abandoned the user's
/// sentence for something out of the background context.
pub fn breaks_word_continuation(completion: &str, prefix: &str) -> bool {
    let mid_word = prefix.chars().last().map_or(false, |c| c.is_lowercase());
    mid_word && completion.chars().next().map_or(false, |c| c.is_uppercase())
}

/// Whether `context` shares at least one significant token (3+ chars, lowercased)
/// with `text` (cotabby ClipboardRelevanceFilter's overlap heuristic): clipboard
/// content that has nothing in common with what the user is writing is noise that
/// steers the completion off the sentence in hand, so it isn't injected at all.
pub fn shares_significant_token(context: &str, text: &str) -> bool {
    fn significant(s: &str) -> std::collections::HashSet<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_lowercase)
            .collect()
    }
    let ctx_tokens = significant(context);
    if ctx_tokens.is_empty() {
        return false;
    }
    let text_tokens = significant(text);
    ctx_tokens.intersection(&text_tokens).next().is_some()
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

    // ── transcript-mimicry filter ─────────────────────────────────────────────

    #[test]
    fn transcript_mimicry_catches_invented_timestamps() {
        assert!(mimics_transcript_format(
            "This war is going to be going on for a long time 7:00 in the evening",
            "I think ",
        ));
        assert!(mimics_transcript_format("see you then 18:45", "ok "));
    }

    #[test]
    fn transcript_mimicry_allows_times_the_user_is_writing_about() {
        assert!(!mimics_transcript_format(
            " and ends at 6:30 pm.",
            "The meeting starts at 5:30 and",
        ));
    }

    #[test]
    fn transcript_mimicry_allows_plain_text_and_ratios() {
        assert!(!mimics_transcript_format("I'm not sure that's a good idea.", "Hmm, "));
        assert!(!mimics_transcript_format("the odds are 3:2 against", "I'd say "));
    }

    // ── preceding-duplication filter ──────────────────────────────────────────

    #[test]
    fn preceding_dup_detects_repeat_of_last_sentence() {
        let prefix = "Let me know if that works. The meeting is at 5pm.";
        assert!(duplicates_preceding_text(" The meeting is at 5pm.", prefix));
    }

    #[test]
    fn preceding_dup_ignores_case_spacing_and_punctuation() {
        let prefix = "Let me know if that works. The meeting is at 5pm.";
        assert!(duplicates_preceding_text("the meeting is at 5 PM", prefix));
    }

    #[test]
    fn preceding_dup_allows_fresh_continuation() {
        let prefix = "Let me know if that works. The meeting is at 5pm.";
        assert!(!duplicates_preceding_text(" I will send the agenda beforehand.", prefix));
    }

    #[test]
    fn preceding_dup_exempts_short_common_phrases() {
        let prefix = "Let me know if that works. The meeting is at 5pm.";
        assert!(!duplicates_preceding_text(" know if", prefix));
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

    #[test]
    fn suppresses_paraphrased_regurgitation() {
        let ctx = InstrContext {
            visual_context: "#BREAKING Trump: We will attack Iran hard tonight".into(),
            ..Default::default()
        };
        // Not a verbatim 4-word run, but every distinctive word came off the screen.
        let out = suppress_context_copy("They will attack Iran hard".into(), &ctx);
        assert_eq!(out, "");
    }

    #[test]
    fn keeps_reply_that_reuses_common_words() {
        let ctx = InstrContext {
            visual_context: "I think that would work well for the new design".into(),
            ..Default::default()
        };
        // "think/that/would/well" are common words — replies naturally reuse them.
        let out = suppress_context_copy("I think that would be fine".into(), &ctx);
        assert_eq!(out, "I think that would be fine");
    }

    // ── insertion safety gate ─────────────────────────────────────────────────

    #[test]
    fn safety_gate_accepts_ordinary_completions() {
        assert!(is_safe_to_insert("hello world"));
        assert!(is_safe_to_insert(")"));
        assert!(is_safe_to_insert("line one\nline two"));
    }

    #[test]
    fn safety_gate_rejects_corruption() {
        assert!(!is_safe_to_insert(""));
        assert!(!is_safe_to_insert("   "));
        assert!(!is_safe_to_insert("bad\u{FFFD}bytes"));
        assert!(!is_safe_to_insert("tab\u{0009}inside"));
        assert!(!is_safe_to_insert("bell\u{0007}"));
    }

    // ── word-continuation coherence ───────────────────────────────────────────

    #[test]
    fn mid_word_prefix_must_be_continued_not_abandoned() {
        assert!(breaks_word_continuation("This war is going on", "not seeing a simila"));
        assert!(!breaks_word_continuation("r view of it", "not seeing a simila"));
        assert!(!breaks_word_continuation(" Sarah and I", "yesterday I met"));
        assert!(!breaks_word_continuation("Anything is possible", "Done. "));
    }

    // ── clipboard relevance ───────────────────────────────────────────────────

    #[test]
    fn clipboard_relevant_when_tokens_overlap() {
        assert!(shares_significant_token(
            "the quarterly report draft",
            "I attached the report you"
        ));
    }

    #[test]
    fn clipboard_irrelevant_when_disjoint_or_empty() {
        assert!(!shares_significant_token(
            "rust compiler error E0382 borrow of moved value",
            "see you at dinner tonight"
        ));
        assert!(!shares_significant_token("", "anything at all"));
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
    fn phrase_loop_cuts_repeated_phrase_to_one_occurrence() {
        // The field-observed failure: "I'm gonna die" cycling.
        let s = "oh my god I'm gonna die I'm gonna die";
        assert_eq!(
            strip_trailing_phrase_loop(s).as_deref(),
            Some("oh my god I'm gonna die")
        );
    }

    #[test]
    fn phrase_loop_is_case_insensitive() {
        let s = "well well I Told You i told you";
        assert_eq!(strip_trailing_phrase_loop(s).as_deref(), Some("well well I Told You"));
    }

    #[test]
    fn phrase_loop_allows_normal_prose() {
        assert_eq!(strip_trailing_phrase_loop("it is what it is"), None);
        assert_eq!(strip_trailing_phrase_loop("the more I see the more I learn"), None);
        assert_eq!(strip_trailing_phrase_loop("a perfectly ordinary sentence"), None);
    }

    #[test]
    fn phrase_loop_respects_punctuation_drift() {
        // "York," != "York" — deliberate repeats carry punctuation that breaks the match.
        assert_eq!(strip_trailing_phrase_loop("New York, New York"), None);
    }

    #[test]
    fn phrase_loop_leaves_numeric_patterns_alone() {
        assert_eq!(strip_trailing_phrase_loop("scores were 2 3 2 3"), None);
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
    fn truncates_hallucinated_turn_after_stop_marker() {
        // Text past a stop marker is a NEW turn the model invented — it must be
        // cut, not spliced onto the real completion by removing the marker.
        let out = normalize_completion(
            "great idea<|im_end|>\n<|im_start|>user what about".into(),
            "that is a ",
        );
        assert_eq!(out, "great idea");
        let out = normalize_completion("done here<end_of_turn>more turn text".into(), "ok ");
        assert_eq!(out, "done here");
    }

    #[test]
    fn keeps_html_strikethrough_close_tag() {
        // </s> is HTML, not a stop marker — truncating on it would cut authoring.
        let out = normalize_completion("<s>old</s> new text".into(), "edit: ");
        assert_eq!(out, "<s>old</s> new text");
    }

    // ── decode-time scaffolding stop ──────────────────────────────────────────

    #[test]
    fn scaffolding_marker_detected_only_for_stop_markers() {
        assert!(contains_scaffolding_marker("done<|im_end|>"));
        assert!(contains_scaffolding_marker("x<end_of_turn>"));
        assert!(!contains_scaffolding_marker("a < b and c > d"));
        assert!(!contains_scaffolding_marker("plain prose"));
    }

    // ── junk punctuation runs (cotabby CompletionSeamGuard) ───────────────────

    #[test]
    fn junk_run_caught() {
        assert!(introduces_junk_punctuation_run("wait....", "I said "));
        assert!(introduces_junk_punctuation_run("$$$$ profit", "we made "));
    }

    #[test]
    fn junk_run_allows_normal_punctuation_and_letters() {
        assert!(!introduces_junk_punctuation_run("done... almost", "nearly "));
        assert!(!introduces_junk_punctuation_run("aaaand done", "")); // letters, not symbols
        assert!(!introduces_junk_punctuation_run("    indented", "")); // whitespace
    }

    #[test]
    fn junk_run_exempts_extending_an_existing_divider() {
        // Continuing the user's own "----" divider is legitimate…
        assert!(!introduces_junk_punctuation_run("----", "section\n----"));
        // …but a sentence merely ending in "." must not exempt "....",
        assert!(introduces_junk_punctuation_run("....", "the end."));
        // and a divider NOT flush against the seam is still junk.
        assert!(introduces_junk_punctuation_run("and ----", "section\n----"));
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
