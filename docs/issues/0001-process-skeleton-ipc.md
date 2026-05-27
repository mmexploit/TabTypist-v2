# 0001 — Process skeleton: Rust core ↔ Swift sidecar JSON-RPC

**Type:** HITL

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

A minimal Rust binary that launches a Swift child process and exchanges JSON-RPC messages over stdin/stdout. Establishes the FFI/IPC pattern that every subsequent slice builds on. This is the foundation; everything later depends on the framing choices made here.

HITL because: the framing choice (raw JSON-RPC vs UniFFI bindings vs a hybrid) and the build-system shape (how the Swift binary is built, where the Rust core finds it, how it ships in the bundle) are decisions to make deliberately, not let drift.

## Acceptance criteria

- [ ] Rust core process spawns the Swift sidecar as a child process
- [ ] JSON-RPC framing (length-prefixed messages or similar) is implemented both sides
- [ ] A `ping` request from Rust returns `pong` from Swift, end-to-end
- [ ] Sidecar shutdown is clean when the parent exits
- [ ] Build pipeline produces both binaries and packages them so the Rust core can find the sidecar at runtime
- [ ] A short ADR captures the IPC framing decision (raw stdio vs UniFFI vs hybrid)

## Blocked by
None — can start immediately.

## User stories addressed
Foundation only; enables every subsequent slice.
