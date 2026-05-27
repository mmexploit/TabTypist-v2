# 0021 — v1 integration polish + tag 1.0

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Final integration pass: pull all preceding slices together, run end-to-end manual test suite, fix any seams that surface, and tag v1.0.

## Acceptance criteria

- [ ] Fresh-install onboarding walks all the way through without an error path being hit
- [ ] English Completions render and accept in Notes, Mail, Slack, VS Code, Chrome textarea
- [ ] Amharic Completions render and accept in the same set of apps
- [ ] Exclusion list correctly suppresses Completions in 1Password, system password prompts, and any default-off banking app on the list
- [ ] Messaging-app first-activation toast fires exactly once per messaging app on a clean install
- [ ] Menu bar paused affordance reflects the current focused app's exclusion state
- [ ] Telemetry is off by default and remains off until the user opts in
- [ ] Sparkle auto-update detects a newer test build and prompts to install
- [ ] DMG download from the website installs without Gatekeeper friction
- [ ] `brew install --cask tabtypist` installs without friction
- [ ] All trust-critical tests (Completion Engine, Exclusion Engine, Downloader, Telemetry Client) pass in CI
- [ ] v1.0 tagged on the main branch; release notes published

## Blocked by
- All previous slices.

## User stories addressed
Final coverage of all PRD user stories not addressed individually elsewhere.
