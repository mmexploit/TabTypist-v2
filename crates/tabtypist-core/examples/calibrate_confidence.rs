// Calibration harness for the instruct confidence floor (average per-token log-probability gate).
//
// Standalone (this crate is binary-only, so examples cannot import its modules): it mirrors
// the production instruct prompt, sampler, and the log-softmax used by `token_logprob`, then
// prints the average per-token log-probability for a few representative cases so we can pick
// a sensible DEFAULT_CONFIDENCE_FLOOR — confident continuations should sit near 0, vague /
// rambling ones noticeably lower.
//
// Usage: cargo run --release --example calibrate_confidence -- /path/to/model.gguf

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use std::num::NonZeroU32;

struct Case {
    name: &'static str,
    screen: &'static str,
    prefix: &'static str,
}

fn build_prompt(screen: &str, prefix: &str) -> String {
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
    if !screen.is_empty() {
        sections.push(String::new());
        sections.push("Background (reference only — do NOT reply to any of this):".into());
        sections.push("Nearby on screen (loose topic hint only):".into());
        sections.push(screen.replace('\n', " "));
    }
    sections.push(String::new());
    sections.push("The user has typed (continue from the end, do not repeat it):".into());
    sections.push(prefix.to_string());
    let body = sections.join("\n");
    format!("<|turn>user\n{body}<turn|>\n<|turn>model\n")
}

fn token_logprob(logits: &[f32], id: usize) -> f32 {
    if id >= logits.len() {
        return 0.0;
    }
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sumexp = 0f32;
    for &l in logits {
        sumexp += (l - max).exp();
    }
    logits[id] - (max + sumexp.ln())
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: calibrate_confidence <gguf>");
    let backend = LlamaBackend::init()?;
    let model = LlamaModel::load_from_file(
        &backend,
        &path,
        &llama_cpp_2::model::params::LlamaModelParams::default().with_n_gpu_layers(99),
    )?;

    let cases = vec![
        Case { name: "email-clear", screen: "From: Sarah Chen. Can you send the finalized Q3 revenue report by Friday?", prefix: "Sure, I'll" },
        Case { name: "telegram-complete", screen: "Is it a python project. It struggles to load extensions for that", prefix: "I think it is not working properly" },
        Case { name: "code-review", screen: "Reviewer: this introduces a race condition in the token refresh", prefix: "Good catch, the race happens because" },
        Case { name: "no-context-vague", screen: "", prefix: "I was thinking that we" },
        Case { name: "midword", screen: "Standup bot: what did you work on yesterday?", prefix: "Yesterday I finished the auth migr" },
        Case { name: "random-noise", screen: "weather sports stocks 42 19 menu file edit view", prefix: "The quarterly" },
    ];

    let turn_close = model.str_to_token("<turn|>", AddBos::Never).ok().filter(|t| t.len() == 1).map(|t| t[0]);

    for c in &cases {
        let prompt = build_prompt(c.screen, c.prefix);
        let tokens = model.str_to_token(&prompt, AddBos::Always)?;
        let mut ctx = model.new_context(
            &backend,
            LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(4096).unwrap())),
        )?;
        let last = tokens.len() - 1;
        let mut batch = LlamaBatch::new(tokens.len().max(512), 1);
        for (i, &t) in tokens.iter().enumerate() {
            batch.add(t, i as i32, &[0], i == last)?;
        }
        ctx.decode(&mut batch)?;

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::penalties(128, 1.3, 0.0, 0.0),
            LlamaSampler::temp(0.2),
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::greedy(),
        ]);

        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut out = String::new();
        let mut pos = tokens.len() as i32;
        let mut sum_lp = 0f64;
        let mut n_lp = 0usize;

        let mut tok = sampler.sample(&mut ctx, last as i32);
        let mut cur_lp = token_logprob(ctx.get_logits_ith(last as i32), tok.0 as usize);
        sampler.accept(tok);

        for _ in 0..30 {
            if tok == model.token_eos() { break; }
            if turn_close.map_or(false, |id| tok == id) { break; }
            sum_lp += cur_lp as f64;
            n_lp += 1;
            let piece = match model.token_to_piece(tok, &mut decoder, false, None) {
                Ok(p) => p,
                Err(_) => break,
            };
            out.push_str(&piece);
            if out.ends_with(|ch: char| matches!(ch, '.' | '!' | '?' | '\n')) { break; }
            let mut b = LlamaBatch::new(1, 1);
            b.add(tok, pos, &[0], true)?;
            ctx.decode(&mut b)?;
            pos += 1;
            tok = sampler.sample(&mut ctx, 0);
            cur_lp = token_logprob(ctx.get_logits_ith(0), tok.0 as usize);
            sampler.accept(tok);
        }

        let avg = if n_lp > 0 { sum_lp / n_lp as f64 } else { 0.0 };
        println!(
            "{:<20} avg_logprob={:>7.3}  n={:>2}  out={:?}",
            c.name, avg, n_lp, out.trim()
        );
    }
    Ok(())
}
