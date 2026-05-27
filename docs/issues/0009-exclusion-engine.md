# 0009 — Exclusion Engine + tests

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

PRD module #4 — Exclusion Engine. Three-tier verdict function:

- **Always-off:** `AXSecureTextField`, OS keychain prompts, any field flagged password-style
- **Default-off, user-overridable:** 1Password, Bitwarden, Dashlane, Keeper, Apple Passwords; curated major US/EU banking app bundle IDs
- **Default-on, user-overridable:** everything else

The sidecar reports focused-app bundle ID and field type with each focus change; Rust calls the engine before triggering a Completion. User overrides (from settings) take precedence within their tier. Includes module #4 test suite.

## Acceptance criteria

- [ ] Verdict function returns off for `AXSecureTextField` unconditionally, even with user override attempting on
- [ ] Verdict function returns off by default for the password-manager bundle IDs in the list
- [ ] User override from settings re-enables a default-off app (verified end-to-end)
- [ ] User override disables a default-on app (verified end-to-end)
- [ ] When verdict is off, no Trigger fires and no overlay renders
- [ ] Menu bar icon reflects paused state when current app is off (UI wiring in #0012; behavior verifiable via logs here)
- [ ] Unit tests cover the verdict matrix and override precedence

## Blocked by
- #0004

## User stories addressed
- 15, 16, 17
