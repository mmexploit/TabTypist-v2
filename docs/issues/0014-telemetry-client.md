# 0014 — Telemetry Client: opt-in plumbing + tests

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

PRD module #7 — Telemetry Client. Opt-in only, off by default. Strict "no transmit before consent" invariant. Collects: app version, OS version, model in use, anonymous completion-acceptance counters, crash stack traces. Never collects: completion text, field text, app names beyond the four messaging apps, any user identifier beyond a randomly-generated install ID resettable from settings. Batches events; flushes on a timer or at app quit. Includes module #7 test suite.

## Acceptance criteria

- [ ] No HTTP request is made before settings record consent as true (verified via network double)
- [ ] Revoking consent halts in-flight batches and clears queued events
- [ ] No event contains completion text or field text (verified via schema and tests)
- [ ] Install ID reset from settings produces a new random ID on the next event
- [ ] Onboarding checkbox writes consent state into Settings (#0006); copy is honest ("Send anonymous crash reports and basic usage metrics to improve TabTypist")
- [ ] Settings toggle reflects and changes consent state at any time
- [ ] Crash reports captured via panic hook in Rust, signal handlers as needed; sent only when consent is true
- [ ] Unit tests cover: consent invariant, revoke-clears-queue, payload schema redaction, install-ID-reset

## Blocked by
- #0006, #0013

## User stories addressed
- 26, 27, 28
