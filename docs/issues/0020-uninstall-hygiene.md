# 0020 — Uninstall hygiene

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

When TabTypist is uninstalled (dragged to Trash or removed via Homebrew cask), all app-owned data is removed: downloaded model files, settings, telemetry install ID, exclusion-list user overrides, onboarding state, Sparkle update cache. A "Reset TabTypist" option in settings produces the same effect without uninstalling, useful for users who want a fresh state.

Settings/model retention across uninstall+reinstall is **off by default** — clean uninstall means clean reinstall.

## Acceptance criteria

- [ ] Dragging the app to Trash + emptying Trash leaves no TabTypist files under `~/Library/Application Support`, `~/Library/Caches`, `~/Library/Preferences`, or `~/Library/LaunchAgents`
- [ ] Homebrew cask uninstall removes the same set of files
- [ ] Settings panel "Reset TabTypist" performs an in-app data wipe and exits the app
- [ ] Reinstalling after uninstall produces a fresh onboarding experience (no leftover state)
- [ ] Model files (potentially several GB) are reliably removed — verified by inspecting disk after uninstall

## Blocked by
- #0006, #0007

## User stories addressed
- 33
