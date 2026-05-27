# 0010 — Remote-signed exclusion-config fetch + verify + apply

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Per PRD "Further Notes," the Exclusion list ships with a signed remote-config update mechanism so we can respond to incidents without an app release. On startup and on a periodic interval (~24h), the app fetches a signed config bundle from a known URL, verifies the Ed25519 signature against a baked-in public key, and atomically replaces the in-memory exclusion list. Tampered or invalid configs are rejected and the previous list remains in effect.

## Acceptance criteria

- [ ] Config bundle is fetched from a configured URL on launch and on a 24h interval
- [ ] Ed25519 signature is verified against a public key compiled into the binary
- [ ] Invalid signature → config is rejected; previous in-memory list remains; failure is logged
- [ ] Valid signature → config replaces the in-memory list atomically (no torn reads)
- [ ] Cached last-known-good config persists across launches when offline
- [ ] User overrides are preserved across remote-config updates (they layer on top, not replace)

## Blocked by
- #0009

## User stories addressed
Trust posture; no direct user story 1:1 — supports the entire Exclusion-list policy.
