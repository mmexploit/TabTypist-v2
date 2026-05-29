use anyhow::{Context, Result};
use encoding_rs;
use std::num::NonZeroU32;
use std::path::Path;

/// A loaded model that can produce completions.
pub trait Completer: Send + Sync {
    fn complete(&self, prefix: &str, suffix: &str, max_tokens: u32) -> Result<String>;
}

// ── llama.cpp implementation ──────────────────────────────────────────────────

pub struct LlamaCppCompleter {
    model: llama_cpp_2::model::LlamaModel,
    backend: llama_cpp_2::llama_backend::LlamaBackend,
}

impl LlamaCppCompleter {
    pub fn load(model_path: &Path) -> Result<Self> {
        use llama_cpp_2::llama_backend::LlamaBackend;
        use llama_cpp_2::model::params::LlamaModelParams;

        let backend = LlamaBackend::init()?;

        let mut model_params = LlamaModelParams::default();
        model_params = model_params.with_n_gpu_layers(99);

        let model = llama_cpp_2::model::LlamaModel::load_from_file(
            &backend,
            model_path,
            &model_params,
        )
        .with_context(|| format!("loading model from {}", model_path.display()))?;

        Ok(Self { model, backend })
    }
}

impl Completer for LlamaCppCompleter {
    fn complete(&self, prefix: &str, suffix: &str, max_tokens: u32) -> Result<String> {
        use llama_cpp_2::context::params::LlamaContextParams;
        use llama_cpp_2::llama_batch::LlamaBatch;
        use llama_cpp_2::model::AddBos;
        use llama_cpp_2::sampling::LlamaSampler;

        const N_CTX: usize = 2048;

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(NonZeroU32::new(N_CTX as u32).unwrap()))
            .with_n_batch(512);

        let mut ctx = self.model.new_context(&self.backend, ctx_params)?;

        // ── Prompt assembly ───────────────────────────────────────────────────
        //
        // Use Fill-in-the-Middle (FIM) when the user has text after the cursor.
        // Qwen2.5 base was trained with FIM natively, so the model understands
        // what comes after the cursor and generates a coherent middle section.
        //
        // IMPORTANT: we tokenize prefix and suffix SEPARATELY, then truncate each
        // before assembling. Tokenizing the assembled FIM string and then
        // truncating from the front would destroy the <|fim_prefix|> marker.

        let token_stream: Vec<llama_cpp_2::token::LlamaToken> = if !suffix.is_empty() {
            // Resolve the three FIM special token IDs.
            let fim_prefix_id = self
                .model
                .str_to_token("<|fim_prefix|>", AddBos::Never)
                .ok()
                .and_then(|t| t.into_iter().next());
            let fim_suffix_id = self
                .model
                .str_to_token("<|fim_suffix|>", AddBos::Never)
                .ok()
                .and_then(|t| t.into_iter().next());
            let fim_middle_id = self
                .model
                .str_to_token("<|fim_middle|>", AddBos::Never)
                .ok()
                .and_then(|t| t.into_iter().next());

            if let (Some(fp), Some(fs), Some(fm)) = (fim_prefix_id, fim_suffix_id, fim_middle_id) {
                // Tokenize prefix (no BOS — the FIM marker acts as sentence start).
                let mut prefix_tokens = self
                    .model
                    .str_to_token(prefix, AddBos::Never)
                    .with_context(|| "tokenizing prefix (FIM)")?;

                // Cap suffix at 256 tokens to leave room for the prefix context.
                let mut suffix_tokens = self
                    .model
                    .str_to_token(suffix, AddBos::Never)
                    .with_context(|| "tokenizing suffix (FIM)")?;
                const SUFFIX_CAP: usize = 256;
                if suffix_tokens.len() > SUFFIX_CAP {
                    suffix_tokens.truncate(SUFFIX_CAP);
                }

                // Budget: N_CTX - max_tokens - 3 FIM markers - suffix
                let prefix_budget =
                    N_CTX.saturating_sub(max_tokens as usize + 3 + suffix_tokens.len() + 4);
                if prefix_tokens.len() > prefix_budget {
                    let drop = prefix_tokens.len() - prefix_budget;
                    prefix_tokens = prefix_tokens[drop..].to_vec();
                }

                // Assemble: [fim_prefix] prefix_tokens [fim_suffix] suffix_tokens [fim_middle]
                let mut tokens = Vec::with_capacity(
                    1 + prefix_tokens.len() + 1 + suffix_tokens.len() + 1,
                );
                tokens.push(fp);
                tokens.extend_from_slice(&prefix_tokens);
                tokens.push(fs);
                tokens.extend_from_slice(&suffix_tokens);
                tokens.push(fm);
                tokens
            } else {
                // FIM tokens not in this model's vocabulary — fall through to prefix-only.
                tracing::warn!("FIM tokens not found in vocab; falling back to prefix-only");
                let mut tokens = self
                    .model
                    .str_to_token(prefix, AddBos::Always)
                    .with_context(|| "tokenizing prefix (fallback)")?;
                let max_prefix = N_CTX.saturating_sub(max_tokens as usize + 4);
                if tokens.len() > max_prefix {
                    let drop = tokens.len() - max_prefix;
                    tokens = tokens[drop..].to_vec();
                }
                tokens
            }
        } else {
            // No suffix — simple prefix continuation (most common case).
            let mut tokens = self
                .model
                .str_to_token(prefix, AddBos::Always)
                .with_context(|| "tokenizing prefix")?;
            let max_prefix = N_CTX.saturating_sub(max_tokens as usize + 4);
            if tokens.len() > max_prefix {
                let drop = tokens.len() - max_prefix;
                tokens = tokens[drop..].to_vec();
            }
            tokens
        };

        if token_stream.is_empty() {
            return Ok(String::new());
        }

        // ── Prefill ───────────────────────────────────────────────────────────

        let mut batch = LlamaBatch::new(512, 1);
        let last_idx = (token_stream.len() - 1) as i32;
        for (i, &tok) in token_stream.iter().enumerate() {
            batch.add(tok, i as i32, &[0], i as i32 == last_idx)?;
        }
        ctx.decode(&mut batch)?;

        // ── Resolve FIM stop tokens ───────────────────────────────────────────
        //
        // In FIM mode the model may emit <|fim_pad|> or <|endoftext|> to signal
        // that the middle section is complete. Stop on these in addition to EOS
        // so the generated text doesn't bleed past its natural boundary.

        let fim_pad_id = self
            .model
            .str_to_token("<|fim_pad|>", AddBos::Never)
            .ok()
            .and_then(|t| t.into_iter().next());
        let endoftext_id = self
            .model
            .str_to_token("<|endoftext|>", AddBos::Never)
            .ok()
            .and_then(|t| t.into_iter().next());

        // ── Sampler ───────────────────────────────────────────────────────────
        //
        // penalties(64, 1.1, 0, 0): light repeat penalty over last 64 tokens.
        //   Necessary on the base model (no instruction fine-tune to suppress loops).
        // min_p(0.05, 1): better nucleus than top_p for prose — dynamically keeps
        //   tokens above 5% of the max-probability token's weight.
        // greedy: deterministic pick from the filtered set (consistency matters
        //   more than variety for a typing assistant).

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::penalties(64, 1.1, 0.0, 0.0),
            LlamaSampler::temp(0.1),
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::greedy(),
        ]);

        // ── Decode loop ───────────────────────────────────────────────────────

        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut result = String::new();
        let mut n_cur = token_stream.len() as i32;

        let mut token = sampler.sample(&ctx, last_idx);
        sampler.accept(token);

        for _ in 0..max_tokens {
            if token == self.model.token_eos() {
                break;
            }
            if fim_pad_id.map_or(false, |id| token == id) {
                break;
            }
            if endoftext_id.map_or(false, |id| token == id) {
                break;
            }

            // special=false skips special tokens so they don't appear in output
            // or trigger a false sentence-boundary break.
            let piece = self
                .model
                .token_to_piece(token, &mut decoder, false, None)?;

            if !piece.is_empty() {
                result.push_str(&piece);
                if ends_at_sentence_boundary(&result) {
                    break;
                }
            }

            let mut next_batch = LlamaBatch::new(1, 1);
            next_batch.add(token, n_cur, &[0], true)?;
            ctx.decode(&mut next_batch)?;
            n_cur += 1;

            token = sampler.sample(&ctx, 0);
            sampler.accept(token);
        }

        let trimmed = result.trim_start().to_string();
        tracing::debug!("completion raw={:?}", trimmed);
        Ok(truncate_at_sentence_boundary(trimmed))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ends_at_sentence_boundary(text: &str) -> bool {
    text.ends_with(|c| matches!(c, '.' | '!' | '?' | '\n'))
}

pub fn truncate_at_sentence_boundary(mut text: String) -> String {
    if let Some(pos) = text.find(|c| matches!(c, '.' | '!' | '?' | '\n')) {
        text.truncate(pos + 1);
    }
    text.trim_end().to_string()
}

// ── Stub completer for tests ──────────────────────────────────────────────────

#[cfg(test)]
pub struct StubCompleter {
    pub response: String,
}

#[cfg(test)]
impl Completer for StubCompleter {
    fn complete(&self, _prefix: &str, _suffix: &str, _max_tokens: u32) -> Result<String> {
        Ok(self.response.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
