# TabTypist v0.1.0 — Beta Release Checklist

Target: **v0.1.0 beta** (pre-release). Build forward from this; `v1.0.0` is not the target.

Status legend: `[ ]` pending · `[~]` in progress · `[x]` done

---

## Phase 0 — Code freeze
- [x] **#1 Commit uncommitted work** — suppression guards, OCR hygiene, perf pass, adaptive debounce across `AXMonitor.swift`, `VisualContextCapture.swift`, `OCRHygiene.swift`, `main.rs`, `model_runtime.rs`.
- [x] **#2 Verify build + tests green** — `cargo test --workspace` (110 tests) and `swift build -c release`.
- [x] **#3 Clean up cargo check warnings** — resolve the 10 pre-existing unused-import / dead-code warnings, or `#[allow]` with intent.

## Phase 1 — Release blockers
- [x] **#4 Add AppIcon.icns** — Info.plist references `AppIcon` but no `.icns` exists. Create it, place in `Resources/`, copy in `bundle.sh`.
- [x] **#5 Add LICENSE file (FSL-1.1)** — Info.plist claims Functional Source License 1.1 but no LICENSE at root. Relevant to monetization optionality.
- [x] **#6 Set version to 0.1.0 beta + fix copyright** — `CFBundleShortVersionString=0.1.0`, align `CFBundleVersion` and Cargo crate version, mark beta where surfaced; copyright 2024 → 2026. Tag will be `v0.1.0`.

## Phase 2 — Features / fixes for beta
- [ ] **#13 Fix model-switching flow** — "Change model…" (`MenuBarController.swift:51`, `SettingsWindowController.swift:170`) calls `OnboardingController.shared.showIfNeeded()`, restarting the full onboarding at the "Get Started" screen. Add a dedicated model-picker entry point that skips welcome/permissions/intro.
- [ ] **#7 Implement check-for-updates** — Integrate Sparkle (SPM dep, EdDSA keypair, real `SUPublicEDKey`, host `appcast.xml`) and add a "Check for Updates…" action. Info.plist already has `SUPublicEDKey` (placeholder) + `SUFeedURL`.
- [ ] **#15 Rebrand model names** — UI must not state the raw model name. Keep real names (Qwen3/Gemma) internally; show branded tier names + size to users. Touch `model_downloader.rs` `display_name`, `OnboardingFlow.swift` `ModelTierInfo.displayName`, `MenuBarController.swift` `modelLoaded()`. *(Brand naming scheme TBD.)*
- [ ] **#16 Redesign Settings UI** — cleaner approach in `SettingsWindowController.swift`. Sections: model, permissions, behavior, updates, privacy/telemetry, about. Coordinates with #13 and #7.
- [ ] **#14 Rename competitor references** — 55 refs to cotabby/cotypist/keytype across 9 files (`AXMonitor.swift`, `VisualContextCapture.swift`, `PopupCardWindow.swift`, `OCRHygiene.swift`, `model_downloader.rs`, `model_runtime.rs`, `main.rs`, `docs/roadmap-competitive-parity.md`, `docs/issues/0040-visual-context-ocr.md`). Describe techniques generically; keep AGPL-clean (concepts only).
- [ ] **#17 Add README** — what TabTypist is, install/permissions (Accessibility + Input Monitoring), architecture (Swift app + Rust core), build (`scripts/bundle.sh`, `make-signing-cert.sh`), license, beta status.

## Phase 3 — Backend / hosted services
- [ ] **#8 Verify telemetry endpoint + consent** — confirm `https://telemetry.tabtypist.com/v1/events` exists; telemetry opt-in with clear consent.
- [ ] **#9 Verify model download flow for fresh user** — 6 GGUFs from HuggingFace `mradermacher/*` require an hf.co token. Verify onboarding HF-token flow on a clean install, or self-host the GGUFs.

## Phase 4 — Signing & notarization
- [ ] **#10 Confirm CI signing/notarization + dry-run** — secrets set (`DEVELOPER_ID_APPLICATION_CERT_P12_BASE64`, `..._PASSWORD`, `NOTARIZE_APPLE_ID`, `NOTARIZE_TEAM_ID`, `NOTARIZE_APP_PASSWORD`); hardened-runtime entitlements survive notarization (JIT / unsigned-mem / disable-library-validation); dry-run notarize+staple once.

## Phase 5 — Clean-machine QA
- [ ] **#11 Clean-machine QA smoke test** — install DMG on a clean Mac/user; grant Accessibility + Input Monitoring; finish onboarding; download a model; test focus → ghost text → Tab accept → Esc dismiss; verify new guards (phrase-loop, junk-run, scaffolding stop, OCR cache) in the wild.

## Phase 6 — Cut the release
- [ ] **#12 Cut the v0.1.0 beta release** — tag `v0.1.0` + push → CI build/sign/notarize/GitHub pre-release with DMG; verify it opens without Gatekeeper warning on a clean machine.
