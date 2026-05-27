# 0016 — Code signing + notarization in CI

**Type:** HITL

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

GitHub Actions workflow that builds the Rust core, builds the Swift sidecar and SwiftUI shell, packages them into a `.app` bundle, signs with the Apple Developer ID Application certificate, notarizes via `notarytool`, staples the ticket, and produces a DMG artifact ready for release.

**HITL because:** requires an Apple Developer account ($99/year) and the secure storage of signing credentials as GitHub Actions secrets — both human-coordination items, not pure engineering.

## Acceptance criteria

- [ ] Apple Developer ID Application certificate is provisioned and stored as a GHA secret
- [ ] Apple Developer ID Installer (or DMG) signing certificate provisioned
- [ ] `notarytool` API key issued and stored as a GHA secret
- [ ] GHA workflow builds, signs, notarizes, and staples on every tagged release
- [ ] Notarized DMG opens without Gatekeeper warnings on a clean macOS install
- [ ] Build secrets never appear in logs (verified)
- [ ] Workflow runs on `release` tag push and is reproducible

## Blocked by
None — can begin in parallel with engineering slices, gated only on Apple Developer enrollment.

## User stories addressed
- 29
