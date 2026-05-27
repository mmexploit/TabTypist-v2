use anyhow::{Context, Result};
use encoding_rs;
use std::num::NonZeroU32;
use std::path::Path;

/// A loaded model that can produce completions.
pub trait Completer: Send + Sync {
    fn complete(&self, prefix: &str, max_tokens: u32) -> Result<String>;
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
    fn complete(&self, prefix: &str, max_tokens: u32) -> Result<String> {
        use llama_cpp_2::context::params::LlamaContextParams;
        use llama_cpp_2::llama_batch::LlamaBatch;
        use llama_cpp_2::model::AddBos;
        use llama_cpp_2::sampling::LlamaSampler;

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(NonZeroU32::new(512).unwrap()))
            .with_n_batch(512);

        let mut ctx = self.model.new_context(&self.backend, ctx_params)?;

        let tokens = self
            .model
            .str_to_token(prefix, AddBos::Always)
            .with_context(|| "tokenizing prefix")?;

        if tokens.is_empty() {
            return Ok(String::new());
        }

        let mut batch = LlamaBatch::new(512, 1);
        let last_idx = (tokens.len() - 1) as i32;
        for (i, &tok) in tokens.iter().enumerate() {
            batch.add(tok, i as i32, &[0], i as i32 == last_idx)?;
        }
        ctx.decode(&mut batch)?;

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(0.1),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::greedy(),
        ]);

        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut result = String::new();
        let mut n_cur = tokens.len() as i32;

        for _ in 0..max_tokens {
            let token = sampler.sample(&ctx, n_cur - 1);
            sampler.accept(token);

            if token == self.model.token_eos() {
                break;
            }

            let piece = self
                .model
                .token_to_piece(token, &mut decoder, true, None)?;
            result.push_str(&piece);

            if ends_at_sentence_boundary(&result) {
                break;
            }

            let mut next_batch = LlamaBatch::new(1, 1);
            next_batch.add(token, n_cur, &[0], true)?;
            ctx.decode(&mut next_batch)?;
            n_cur += 1;
        }

        Ok(truncate_at_sentence_boundary(result))
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
    fn complete(&self, _prefix: &str, _max_tokens: u32) -> Result<String> {
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
