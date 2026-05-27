# 0018 — Homebrew cask formula

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Publish a Homebrew cask formula so `brew install --cask tabtypist` installs the latest notarized DMG from the release feed. Cask lives either in homebrew-cask itself (if accepted) or in a TabTypist tap repo (`homebrew-tap` under the project's GitHub org).

## Acceptance criteria

- [ ] Cask formula validates locally via `brew audit --strict --cask`
- [ ] Cask installs the current notarized release without Gatekeeper friction
- [ ] Cask uninstall removes the app cleanly (defers to #0020 for full data hygiene)
- [ ] Formula updates automatically on each new release via a small CI job that bumps the SHA + version
- [ ] README in the tap repo documents installation

## Blocked by
- #0016

## User stories addressed
- 31
