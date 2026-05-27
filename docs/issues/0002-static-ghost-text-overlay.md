# 0002 — Static ghost-text overlay at fixed point

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Swift sidecar opens a borderless `NSPanel` at a hardcoded screen coordinate and renders fixed ghost-text. Rust core sends a `showOverlay(x, y, text)` message; sidecar shows the panel. A `hideOverlay` message hides it. Establishes the overlay-rendering primitive — actual caret tracking and real completions come later.

## Acceptance criteria

- [ ] Borderless `NSPanel` renders above all other windows, click-through enabled
- [ ] Ghost-text styling is faded (low-contrast, italic or matching app text style)
- [ ] `showOverlay` and `hideOverlay` JSON-RPC messages work
- [ ] Panel does not steal focus from the underlying app
- [ ] Panel does not appear in Mission Control / Cmd-Tab

## Blocked by
- #0001

## User stories addressed
- 6 (foundation for ghost-text rendering)
