# Feature Parity Roadmap

Derived from industry research and competitive analysis.
Each item is a discrete implementation unit. Items within a group are
loosely ordered by dependency; groups are ordered by user impact.

---

## Group 1 — Inference quality & latency

| # | Item | Notes |
|---|---|---|
| 1.1 | **KV cache reuse** ✅ | Persistent inference thread; delta-decode on extension, full re-prefill on diverge. Implemented, compiles. |
| 1.2 | **Debounce 250 ms → 20 ms** ✅ | Updated 2026-06: Cotabby 2 ships 20 ms paired with cooperative mid-decode cancellation; we match both. Cancellation flag lives on the completer (`cancel_handle`), checked per token. |
| 1.2b | **Trigger threshold: any non-whitespace character** | Lower `should_trigger_completion` from "≥ 1 complete word" to "any non-whitespace character in the current field." Industry standard — both major competing apps fire on the first character. Viable now that KV cache makes a triggered-but-stale inference cheap (delta decode only). Change in `main.rs`: replace word-count guard with `!prefix.trim().is_empty()`. |
| 1.3 | **Echo suppression** ✅ | Strip words from completion start that repeat the tail of the prefix. Fixes the "cycling words" symptom. Word-by-word overlap search: find longest suffix of preceding text that matches a prefix of the completion, strip it. Implement in Rust `model_runtime` normalizer. |
| 1.7 | **Common-prefix KV reuse** ✅ | (2026-06) Trim KV cache to the longest common token prefix and decode only the remainder — covers deletions, window slides, and crucially the instruct path, which previously re-prefilled the whole prompt every keystroke. Ported from Cotabby `LlamaRuntimeCore.obtainAutocompleteSequence`. |
| 1.8 | **Prompt prefix windowing** ✅ | (2026-06) Last 1 000 chars → last 50 words, joined by single spaces, trailing whitespace trimmed (Cotabby `truncatedPromptPrefix` + `BaseCompletionPromptRenderer`). Bounds prefill latency and stops stale context steering output. |
| 1.9 | **Cotabby sampler chain** ✅ | (2026-06) penalties(64, 1.05) → top-k 20 → top-p 0.7 → min-p 0.08 → temp 0.1 → dist(fixed seed 0xC0FFEE). Replaces greedy + 1.1/1.3 repeat penalties; fixed seed = reproducible ghost text. |
| 1.10 | **Sentence-boundary classifier** ✅ | (2026-06) Early decode stop + truncation no longer fire on decimals ("3.14"), list numbers, single-letter initials ("U.S."), or abbreviations (e.g., etc., Dr.); walks back closing quotes/brackets; min 2 tokens before early stop. |
| 1.11 | **Trailing-duplication filter** ✅ | (2026-06) Suppress completions that mostly retype the text after the caret (folded alphanumeric comparison, 3-char floor, 3 shapes). Ported from Cotabby `TrailingDuplicationFilter`. |
| 1.4 | **Completion normalizer** | Single Rust function applied to every raw model output: (a) strip `<\|im_end\|>` / `<\|im_start\|>` / `<\|im_start\|>assistant` chat tokens; (b) strip `<think>…</think>` reasoning blocks; (c) collapse `\r`; (d) apply echo suppression (1.3); (e) leading-whitespace normalisation. |
| 1.5 | **Token budget presets** | Replace hardcoded `max_tokens: 25` with three tiers: short = 11, medium = 18, long = 30 (default). Expose as a settings key. Formula: `max(config_base, preset_budget)`; multi-line doubles it, cap 60. |
| 1.6 | **Multi-line mode toggle** | Off by default. When on: keep content up to first blank-line boundary instead of first `\n`. Doubles token budget, cap 60. |

---

## Group 2 — Acceptance UX

| # | Item | Notes |
|---|---|---|
| 2.1 | **Full-accept shortcut** | Keybind (default: backtick `` ` ``, the key above Tab) that accepts the entire remaining completion in one keystroke. Tab remains word-by-word. Industry-standard mapping — independently validated as the default in both major competing apps. Fully configurable in settings; note that some international keyboards have `§` or `^` in that position, so configurability is a hard requirement. Requires CGEventTap to intercept the key when a completion is visible. |
| 2.2 | **Auto-accept trailing punctuation toggle** | When off, word acceptance stops before trailing punctuation ("you" not "you?") and punctuation is returned as the next chunk. Default: on. |
| 2.3 | **Keycap hint pill** | Render a small "Tab" pill glyph inline after the ghost text to teach first-time users what to press. Respects ghost text opacity; disappears after N acceptances (onboarding complete). |
| 2.4 | **Text injection: AXUIElementSetAttributeValue** | Try AX set-value first (avoids clipboard clobber); fall back to Cmd+V only when AX write is rejected. Per-app preference cached so we don't retry the slow path repeatedly. |

---

## Group 3 — Overlay & display

| # | Item | Notes |
|---|---|---|
| 3.1 | **AXObserver notifications** | Register `kAXValueChangedNotification` + `kAXSelectedTextRangeChangedNotification` on focus change. Fire poll immediately on notification; keep timer at 80 ms as backstop for apps that don't post notifications. Drop CPU on idle. |
| 3.2 | **Adaptive poll backoff** | When focused field has not changed for N consecutive polls, widen interval up to 200 ms. Reset to 80 ms on any change. |
| 3.3 | **Overlay stability gate** | Suppress re-showing the overlay within a short window after hiding (prevents flicker when AX publishes stale state post-acceptance). |
| 3.4 | **Caret position prediction post-accept** | After Cmd+V injection, the AX caret rect lags by 1–2 polls. Predict the new caret X by measuring average character width from recent AX frames and advancing by `accepted_text.count × char_width`. |
| 3.5 | **Mirror / popup card mode** | For apps where AX caret is unreliable (Firefox, apps with estimated caret geometry), render a floating card near the field bottom instead of inline ghost text. Auto-select mode per app; user can pin. |
| 3.6 | **Ghost text opacity control** | Expose 30–100 % opacity slider in settings (default 40 %). |
| 3.7 | **Custom ghost text colour** | Hex colour picker in settings; falls back to `labelColor` at chosen opacity. |
| 3.8 | **Field-edge indicator icon** | Small icon at the right edge of the focused field showing TabTypist is active. Hide in excluded apps. |

---

## Group 4 — Context & prompt quality

**Combined context budget: 1 000 characters hard cap across all injected fields.**
Priority order (highest signal first — lower items are silently dropped when budget is exhausted):
visual OCR context → app name → language instruction → user name → custom rules → clipboard → typing history excerpts.

| # | Item | Notes |
|---|---|---|
| 4.1 | **App name in prompt** | Pass `appBundleId` display name to Rust; include in instruct-model prompt as "The user is typing in [App]." Improves tone matching. |
| 4.2 | **User name in prompt** | Optional profile name in settings; injected as "The user's name is [Name]." Personalises completions. |
| 4.3 | **Language targeting** | Detect script/language of preceding text; include "The user usually writes in [Language]" hint. Amharic detection already planned. |
| 4.4 | **Custom writing rules** | Free-text user directives ("avoid passive voice", "use em-dashes") appended to instruct prompt. Cap at N rules, normalise/dedup. |
| 4.5 | **Clipboard context (opt-in)** | When enabled, include sanitised clipboard text in prompt. Relevance filter: skip if clipboard is code, binary, or a URL list. Counts against the 1 000-char budget. |
| 4.6 | **Visual context / OCR** | Screen Recording permission (optional — app still works without it, quality degrades gracefully). Screenshot only the region **above** the focused text field (from top of screen to `inputFrame.origin.y`), not the full screen. Run Vision OCR on that region. **Filtering strategy — experiment both:** (B) proximity trim: take the bottom N characters of OCR output (physically closest to the input field), discards toolbars/UI chrome naturally; (C) model distillation: fast summarisation pass via a small local model to strip UI chrome and keep prose — higher quality but adds latency. Ship B as default; run C behind a feature flag and measure end-to-end latency delta. If C adds >100 ms on the default hardware tier, keep B. |
| 4.7 | **Typing history / personal vocabulary** | Local encrypted SQLite db (key in Keychain) of accepted completions and opted-in typed text. At inference time, retrieve relevant phrases and inject as context: "This user often writes: [examples]." Strength slider (Off → Strong) controls injection weight. Two collection modes: accepted-only (default, low noise) and all-inputs. Auto-exclude short form fields, password fields. Enables suggestions that reflect the user's actual vocabulary, names, and turns of phrase. |
| 4.8 | **Per-app custom instructions** | Instructions scoped to a specific app bundle ID or domain, appended after global custom rules (4.4) when TabTypist is active in that app. Enables tone switching: formal instructions for Mail, casual for Messages, German for one app and English for another. |

---

## Group 5 — App compatibility

| # | Item | Notes |
|---|---|---|
| 5.1 | **Electron / web-app support** | Use OCR (4.6) to extract prefix text in apps where `caretHeight = 0`. Render overlay using screen-coordinate caret estimate from Vision framework bounding box. |
| 5.2 | **Terminal support** | Opt-in (default off). Auto-activate for AI agent prompts; detect prompt characters (`$`, `>`, `❯`) and only suggest there. |

---

## Group 6 — Model catalog & backends

| # | Item | Notes |
|---|---|---|
| 6.0 | **Base-model catalog + conditioning preface** ✅ | (2026-06) Re-tiered the catalog around base checkpoints, matching Cotabby 2 (which dropped instruct GGUFs entirely) and Cotypist's base+continuation approach: nano Qwen3-0.6B-Base Q4_K_M (0.4 GB), mini Qwen3.5-0.8B-Base Q6_K (0.6 GB), standard Qwen3.5-2B-Base Q4_K_M (1.3 GB, default), performance Qwen3-4B-Base Q4_K_M (2.5 GB), quality gemma-4-E2B base Q6_K (3.8 GB), pro gemma-4-E4B base Q4_K_M (5.3 GB) — one model per tier, all `ModelKind::Base`, mradermacher GGUFs (mini/standard/quality/pro are Cotabby 2's exact shipped files; sizes verified against the HF API). Base path now prepends a conditioning preface (`base_preface`, port of Cotabby `BaseCompletionPromptRenderer`): persona/style/vocabulary/language/clipboard/screen stated as facts, app name deliberately excluded, prefix last after a blank line; skipped in FIM mode. Instruct inference path retained for user-supplied instruct GGUFs. NOTE: old installed model ids no longer resolve — existing installs re-run the model download in onboarding. |
| 6.1 | **Model tier catalog** | Expand `ModelCatalog` to 6 tiers mixing Qwen3 base and Gemma4 instruct models. User sees all tiers with an override dropdown; onboarding pre-selects based on detected RAM (`sysctl hw.memsize`). Never auto-select upward — let the user opt into higher tiers manually. Validated threshold: 16 GB comfortably runs a 3.1 GB model in production. Tiers: **nano** SmolLM2-135M-Instruct Q8 (0.1 GB, 8 GB RAM), **mini** Qwen3-0.6B Q4_K_M (0.4 GB, 8 GB RAM), **standard** Qwen3-1.7B Q4_K_M (~1 GB, 8 GB RAM), **performance** Qwen3-4B Q4_K_M (~2.3 GB, 16 GB RAM), **quality** Gemma4-E2B-it Q4_K_M (3.1 GB, 16 GB RAM — recommended auto-select for 16 GB), **pro** Gemma4-E4B-it Q4_K_M (5.0 GB, 24 GB+ RAM). Gemma4 tiers use the instruct inference path (6.2); Qwen3 tiers use the base path. |
| 6.2 | **Instruct model inference path** | Separate code path for instruct models: build a system prompt with length instruction + app/user/language context; strip echo and control tokens via normaliser (1.4). Base path (current) unchanged. Route by model type at load time. |
| 6.3 | **HuggingFace model browser** | Settings pane: search HF for GGUF models, browse files, download. Strong differentiator for power users who want to bring their own model. |
| 6.4 | **Apple Intelligence backend** | Use `FoundationModels` framework on macOS 26+. Zero download, Apple-managed privacy. Large context window (4 096 tokens shared). |
| 6.5 | **Hosted / cloud tier** | Already in v2 ADR. Remote inference for Amharic and low-RAM Macs. |

---

## Group 7 — Distribution & polish

| # | Item | Notes |
|---|---|---|
| 7.1 | **Auto-update (Sparkle)** | Already in issues/0015. |
| 7.2 | **Uninstall hygiene** | Already in issues/0020. |
| 7.3 | **Onboarding: model tier picker** | Replace single English model download with tier selector in onboarding. Detect RAM via `sysctl hw.memsize`; pre-select recommended tier (8 GB → standard, 16 GB → quality, 24 GB+ → pro); show full tier list with size and RAM labels so user can override downward or upward. Display expected download size prominently. |
| 7.4 | **Menu bar: active model display** | Show currently loaded model name/tier in menu bar popover. |

---

## Implementation order (suggested)

Fast wins first, deeper infra after:

```
1.2 → 1.3 → 1.4 → 1.5   (inference quality, all Rust)
2.1 → 2.2 → 2.3           (acceptance UX, Swift + Rust)
2.4                         (injection method, Swift)
3.1 → 3.2 → 3.3 → 3.4    (overlay stability, Swift)
6.1 → 6.2                  (model catalog + instruct path)
4.1 → 4.2 → 4.3 → 4.4    (prompt context)
3.5 → 3.6 → 3.7 → 3.8    (display polish)
5.1 → 5.2                  (Electron / terminal)
4.5 → 4.6                  (clipboard + visual context)
6.3 → 6.4 → 6.5           (advanced backends)
7.1 → 7.2 → 7.3 → 7.4    (distribution)
```
