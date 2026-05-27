# 0006 — Settings Store with schema versioning

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

PRD module #6 — schema-versioned settings persistence in the macOS Application Support directory. JSON on disk with a `schema_version` field; migrations run on load when the version is older than the current code expects. At this slice, settings stored are minimal: selected languages, model overrides, telemetry consent, user-edited exclusion entries. The store exposes typed get/set with change notifications so the UI can react.

## Acceptance criteria

- [ ] Settings file lives under `~/Library/Application Support/TabTypist/settings.json`
- [ ] Schema version is recorded; loading an older version triggers in-memory migration without overwriting the file until next save
- [ ] Concurrent reads are safe; writes are atomic (write-temp-then-rename)
- [ ] Change notifications fire on set
- [ ] Settings survive an app restart

## Blocked by
- #0001

## User stories addressed
- 19, 22, 27
