# 0011 — Messaging-app first-activation toast

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Per ADR-0002, the first time TabTypist activates in each of Signal, Telegram, WhatsApp, and iMessage, the user sees a one-time toast: "TabTypist is generating completions here. Local-only by default. Disable for this app?" with "Keep" and "Disable in [App]" actions. State is persisted in Settings so the toast never repeats per-app.

## Acceptance criteria

- [ ] Toast renders the first time the focused app's bundle ID matches one of the four messaging apps
- [ ] "Disable in [App]" writes a user override into the Exclusion list and immediately pauses TabTypist in that app
- [ ] "Keep" records the toast as shown and never re-prompts for that app
- [ ] Per-app toast-shown state survives app restarts
- [ ] Resetting telemetry/install ID does not reset toast state (these are independent)
- [ ] Toast styling matches the design system (await #0013 / `/shape` output for final visual)

## Blocked by
- #0009

## User stories addressed
- 18
