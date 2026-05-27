---
status: accepted
---

# Rust core with native UI shells per platform

TabTypist's roadmap requires both macOS (v1) and Windows-low-end (v1.5+) to be real targets — the differentiating wedge (Amharic + emerging-market hardware) only materializes once Windows ships. We chose a **Rust core** holding all platform-agnostic logic (model orchestration, completion engine, debounce/cancel, ranking, language→model routing, settings) with **thin native UI shells per platform** (Swift + AppKit on macOS, Win32/WinUI on Windows later) over the two main alternatives: native-everything-per-platform (faster v1, but Windows becomes a second-project rewrite that historically never ships) and Tauri (lower technical ceiling, no path to MLX, web-runtime overhead). The decision optimizes for the v1 → v1.5 trajectory rather than v1 in isolation.

## Considered options

- **Native everything per platform (Swift + AppKit + MLX on macOS, separate Win32 app later).** Best v1 latency and AX integration; ~30% faster decode via MLX. Rejected because the Windows port becomes a fresh codebase, which is the realistic cause of "Windows support" never shipping.
- **Tauri (Rust + web UI).** Faster for web-skilled developers, but the ghost-text overlay must be a native window anyway, AX/UIA needs native sidecars, and MLX is effectively unreachable from Rust without an additional Swift sidecar process. The implicit comfort optimization was distorting the technical answer.
- **Rust core + native UI shells.** Chosen. ~60–75% of code shared and written once. Windows port becomes a real port (replace Swift AX sidecar with a UIA sidecar; replace SwiftUI menu bar with WinUI), not a rewrite.

## Consequences

- v1 uses **llama.cpp linked into Rust** via `llama-cpp-2`. MLX is deferred to a later optimization once the architecture is stable — running MLX in a Swift sidecar over IPC would defeat its latency advantage.
- A Rust↔Swift FFI boundary exists from day one (UniFFI or hand-rolled C FFI). The AX integration on macOS lives in a small Swift module exposing `focusedField → (text, caretRect)` and capturing Tab/Escape; Rust drives orchestration.
- v1 ships ~30% slower decode on Apple Silicon than a hypothetical native Swift + MLX version (Qwen 2.5 1.5B 4-bit on Metal-backed llama.cpp: ~80 tok/s on M2; ~310ms for a 25-token completion). Inside the acceptable latency band but visibly less "instant" than MLX would be. Accepted as a v1 trade.
- The macOS app will not be as native-feeling as a pure-Swift competitor (e.g., cotabby) on subtle details. The trade is that Windows is reachable.
