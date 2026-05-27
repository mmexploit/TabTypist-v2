# 0015 — Sparkle 2.x auto-update with EdDSA-signed feed

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

PRD module #12 — Updater. Integrate Sparkle 2.x, host an appcast XML feed on the TabTypist domain, sign every release with EdDSA, and offer automatic updates on the stable channel. Beta channel available for users who opt in from settings.

## Acceptance criteria

- [ ] Sparkle integrated; app checks the appcast on launch and on a daily interval
- [ ] EdDSA-signed update bundles; Sparkle rejects unsigned or wrong-key bundles
- [ ] Stable and beta channels routed by separate appcast URLs; user can switch in settings
- [ ] Update prompt is non-blocking — user can defer or skip a version
- [ ] Updates do not lose Settings, downloaded models, or telemetry consent state
- [ ] Appcast hosting infrastructure documented in `docs/ops/appcast.md`

## Blocked by
- #0016

## User stories addressed
- 29
