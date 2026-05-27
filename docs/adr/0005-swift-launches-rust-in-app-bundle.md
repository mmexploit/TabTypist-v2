---
status: accepted
---

# Swift main app launches Rust core in the app bundle

Ticket 0001 describes "a Rust binary that launches a Swift child process." That framing describes the dev-standalone test workflow (run `tabtypist-core` directly; it spawns the sidecar). In the actual macOS `.app` bundle, the direction is reversed: the Swift binary is the main executable (required for NSApp, menu bar item, NSPanel overlay, Accessibility permission trust binding to the bundle), and it spawns the Rust core as a subprocess helper.

## Consequences

- `TabTypist.app/Contents/MacOS/TabTypist` is the Swift binary (NSApp, UI, AX, key capture).
- `TabTypist.app/Contents/Resources/tabtypist-core` is the Rust binary (inference, settings, downloader).
- IPC is identical in both directions: newline-delimited JSON-RPC over piped stdin/stdout. The IPCBridge can operate in both modes (app mode: FileHandle pipes; standalone mode: process stdin/stdout).
- The macOS Accessibility and Input Monitoring grants are attached to the `com.tabtypist.TabTypist` bundle, not to individual binaries — both processes inherit the trust when they run within that bundle.
