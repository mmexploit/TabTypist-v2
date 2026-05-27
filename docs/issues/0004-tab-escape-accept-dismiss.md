# 0004 — Tab/Escape capture + accept/dismiss (hardcoded completion text)

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Sidecar installs a `CGEventTap` (Input Monitoring) capturing Tab and Escape globally. On Tab while ghost text is visible, the sidecar inserts the completion into the focused field via AX `setValue` and reports the Acceptance. On Escape, sidecar reports a Dismissal and the overlay hides. **Tab passes through to the underlying app when no Completion is showing** (critical for not breaking Tab in code editors and forms).

The completion text is still hardcoded at this stage — replacing it with real model output is slice #0005.

## Acceptance criteria

- [ ] CGEventTap captures Tab and Escape system-wide
- [ ] Tab inserts the ghost-text into the focused field and clears the overlay
- [ ] Escape clears the overlay without inserting
- [ ] When no overlay is showing, Tab passes through to the host app unmodified (verified in VS Code where Tab must still indent)
- [ ] Divergent typing — user types text that no longer prefix-matches the completion — silently dismisses the overlay
- [ ] No completion text is rendered while the overlay is hidden

## Blocked by
- #0003

## User stories addressed
- 6, 7, 8, 9
