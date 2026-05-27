# 0005 — First real Completion via llama.cpp + Completion Engine tests

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Bind `llama-cpp-2` into the Rust core. Load a bundled small model (Qwen 2.5 1.5B 4-bit). Implement the **Completion Engine** (per PRD module #1): debounce (~150ms), in-flight cancellation, sentence-boundary truncation, divergent-typing dismissal, 25-token cap. On every reported caret/text change from the sidecar, the engine debounces, then calls `LlamaCppCompleter.complete(prefix, suffix)`, streams tokens, and sends the assembled Completion to the sidecar for rendering.

Includes the test suite for module #1 per the PRD testing plan.

## Acceptance criteria

- [ ] llama.cpp loads a 4-bit GGUF Qwen 2.5 1.5B from disk
- [ ] Real Completions render in Notes/Mail/etc. within ~300ms of pause
- [ ] Debounce coalesces a burst of keystrokes into one inference
- [ ] In-flight cancellation actually halts the running inference when the trigger changes
- [ ] Sentence-boundary truncation stops at `.!?\n` or at 25 tokens
- [ ] Divergent typing dismisses the in-flight completion
- [ ] Unit tests for the Completion Engine pass against a stub `Completer` and stub `ContextProvider`

## Blocked by
- #0004

## User stories addressed
- 5, 10
