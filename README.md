# TabTypist

Ghost-text inline completions for every app on your Mac — powered by a local AI model. No cloud, no subscriptions.

Start typing anywhere. After a brief pause, a grey suggestion appears at the caret. Press **Tab** to accept or **Esc** to dismiss.

## Features

- Works system-wide: text editors, email clients, chat apps, browsers, anything
- Runs entirely on-device — no text leaves your Mac
- Six model tiers (Nano → Pro) to match your hardware
- Context-aware: reads on-screen text near the field via on-device OCR (optional)
- Configurable completion length, writing style rules, and per-app exclusions

## Requirements

- macOS 14 Ventura or later
- Apple Silicon or Intel Mac with at least 8 GB RAM (4 GB for the Nano tier)

## Permissions

TabTypist needs two permissions to function, and one optional one:

| Permission | Why |
|---|---|
| **Accessibility** | Read caret position; insert text when you press Tab |
| **Input Monitoring** | Detect Tab and Escape keypresses |
| **Screen Recording** *(optional)* | On-device OCR of nearby text for context-aware suggestions |

After installing, grant these in **System Settings → Privacy & Security**.

## Build from Source

### Prerequisites

- Xcode 15+ / Swift 5.9+
- Rust toolchain (stable) — install via [rustup.rs](https://rustup.rs)
- Cargo's target for your architecture (included with Rust)

### Build

```bash
# Clone the repository
git clone https://github.com/tabtypist/TabTypist.git
cd TabTypist

# Build and assemble the .app bundle (debug)
bash scripts/bundle.sh

# Or build a release bundle
bash scripts/bundle.sh --release
```

The assembled app is at `dist/TabTypist.app`.

### Code signing (for development)

Without a stable signing identity, macOS revokes Input Monitoring on every rebuild. Create a self-signed identity once:

```bash
bash scripts/make-signing-cert.sh
```

This creates a "TabTypist Dev" identity in your login keychain. The bundle script finds and uses it automatically on subsequent builds.

## Architecture

TabTypist is two processes communicating over a JSON-RPC pipe:

```
TabTypist (Swift)          tabtypist-core (Rust)
  Menu bar UI       ←──→   llama.cpp inference
  AX monitor                Model downloader
  Overlay / popup           Settings store
  Onboarding UI             Exclusion engine
```

- **Swift app** (`Sources/TabTypist/`): menu bar, onboarding, overlay windows, AX and key capture
- **Rust core** (`crates/tabtypist-core/`): local inference via `llama-cpp-2`, model downloads, settings persistence

The Rust binary lives in `TabTypist.app/Contents/Resources/tabtypist-core`. The Swift app spawns it on launch and communicates over piped stdin/stdout.

## Model Tiers

| Tier | Size | Min RAM |
|---|---|---|
| Nano | 0.4 GB | Any Mac |
| Mini | 0.6 GB | 8 GB+ |
| Standard | 1.3 GB | 8 GB+ |
| Performance | 2.5 GB | 16 GB+ |
| Quality | 3.9 GB | 16 GB+ |
| Pro | 5.3 GB | 24 GB+ |

Models are GGUF base checkpoints downloaded from HuggingFace during onboarding. A HuggingFace account (free read-only token) is required.

## Beta Status

This is a **v0.1.0 pre-release**. Expect rough edges:

- Model download requires a HuggingFace token (self-hosting planned for v1.0)
- Telemetry endpoint is not yet live
- Automatic update delivery (Sparkle) is wired but the appcast is not yet hosted

Bug reports and feedback welcome via [GitHub Issues](https://github.com/tabtypist/TabTypist/issues).

## License

[Functional Source License 1.1](LICENSE) — free for non-production use; converts to Apache 2.0 four years after publication.
