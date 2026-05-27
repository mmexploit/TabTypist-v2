# 0003 — Focused-field caret tracking + overlay follows caret

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Sidecar uses `AXUIElement` to observe the focused text field across all apps, reads the caret rect, and reports it to Rust. The Rust core forwards the rect to the overlay renderer, which positions the `NSPanel` at the caret. Demo: type into Notes, see ghost text follow the cursor.

This slice requires the Accessibility permission to be granted manually for development; production permission flow is in slice #0013.

## Acceptance criteria

- [ ] Sidecar registers an AX observer for system-wide focus changes
- [ ] On focus change, sidecar reports the focused element's bundle ID and caret rect
- [ ] On caret movement within a field, sidecar reports the updated rect
- [ ] Overlay panel position tracks the caret in real time without visible lag
- [ ] Works in at least Notes, Mail, TextEdit, and a Chrome textarea
- [ ] Apps without AX text support (e.g., Terminal) report a clean "no text field" signal rather than crashing

## Blocked by
- #0002

## User stories addressed
- 1, 10
