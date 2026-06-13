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
- [x] **#13 Fix model-switching flow** — Added `ModelPickerController` + `ModelPickerView` in `OnboardingFlow.swift`. "Change model…" in both `MenuBarController` and `SettingsWindowController` now opens the dedicated picker, bypassing welcome/permissions/intro.
- [x] **#7 Implement check-for-updates** — Sparkle 2.x added as SPM dep; EdDSA keypair generated (`SUPublicEDKey` updated in `Info.plist`); `SPUStandardUpdaterController` wired in `AppDelegate`; "Check for Updates…" in menu bar and Settings routes through `checkForUpdatesRequested` notification. `appcast.xml` hosting is Phase 3.
- [x] **#15 Rebrand model names** — `ModelTierInfo.displayName` now shows branded tier names (Nano/Mini/Standard/Performance/Quality/Pro). Raw model family names (Qwen3/Gemma) stay in `id` only. `MenuBarController.modelLoaded` maps `tier` to branded name via `ModelTierInfo.brandedName(for:)`.
- [x] **#16 Redesign Settings UI** — `SettingsWindowController.swift` reorganised: Model → Completion Behavior (length + multi-line, new) → Permissions → Context → Writing → Updates → Privacy → About (with version number).
- [x] **#14 Rename competitor references** — All 55 refs to cotabby/cotypist/keytype replaced with generic technique descriptions across all source files.
- [x] **#17 Add README** — `README.md` added: what TabTypist is, permissions table, build instructions, architecture diagram, model tier table, beta status, license.

## Phase 3 — Backend / hosted services
- [ ] **#8 Verify telemetry endpoint + consent** — confirm `https://telemetry.tabtypist.com/v1/events` exists; telemetry opt-in with clear consent.
- [ ] **#9 Verify model download flow for fresh user** — 6 GGUFs from HuggingFace `mradermacher/*` are on public ungated repos (no token required; verified via 302→public CDN). Verify onboarding download works on a clean install. Self-hosting GGUFs is still preferred for reliability.

## Phase 4 — Signing & notarization
- [ ] **#10 Confirm CI signing/notarization + dry-run** — secrets set (`DEVELOPER_ID_APPLICATION_CERT_P12_BASE64`, `..._PASSWORD`, `NOTARIZE_APPLE_ID`, `NOTARIZE_TEAM_ID`, `NOTARIZE_APP_PASSWORD`); hardened-runtime entitlements survive notarization (JIT / unsigned-mem / disable-library-validation); dry-run notarize+staple once.

## Phase 5 — Clean-machine QA
- [ ] **#11 Clean-machine QA smoke test** — install DMG on a clean Mac/user; grant Accessibility + Input Monitoring; finish onboarding; download a model; test focus → ghost text → Tab accept → Esc dismiss; verify new guards (phrase-loop, junk-run, scaffolding stop, OCR cache) in the wild.

## Phase 6 — Cut the release
- [ ] **#12 Cut the v0.1.0 beta release** — tag `v0.1.0` + push → CI build/sign/notarize/GitHub pre-release with DMG; verify it opens without Gatekeeper warning on a clean machine.
