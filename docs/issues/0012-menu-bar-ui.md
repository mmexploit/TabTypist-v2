# 0012 — Menu bar UI: status icon, paused affordance, per-app disable

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

PRD module #10 — Menu Bar UI. SwiftUI menu bar status item with two visual states: **active** (default) and **paused** (when the Exclusion Engine returns off for the focused app). Dropdown menu shows: current focused app name and TabTypist's state in it, a "Disable in [App]" / "Enable in [App]" toggle, link to Settings, link to Quit.

## Acceptance criteria

- [ ] Status item appears in the menu bar after first launch
- [ ] Icon changes between active and paused states based on Exclusion verdict for the focused app
- [ ] Dropdown reflects the current focused app name and state
- [ ] "Disable in [App]" writes a user override and the icon updates to paused immediately
- [ ] "Enable in [App]" reverses the override
- [ ] Settings link opens the settings window (window itself is a stub at this slice; full settings is part of #0013/#0017)
- [ ] Quit cleanly shuts down both the Rust core and the Swift sidecar

## Blocked by
- #0009

## User stories addressed
- 4, 19, 20
