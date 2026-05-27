# 0013 — Onboarding flow: two-phase progressive sequence

**Type:** HITL

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

PRD module #11 — Onboarding Flow. Two-phase progressive sequence per ADR-0002 schedule:

1. Welcome + value pitch (skippable)
2. Language selection (English preselected; Amharic option; "Advanced" disclosure for model override)
3. Accessibility permission request with honest copy
4. Background model download starts; user reads a "how TabTypist works" interstitial during the download
5. After Accessibility grant + first focused-field detection, render a tooltip: "Press Tab to accept, Escape to dismiss"
6. After the user's first Acceptance, prompt for Input Monitoring permission with the now-established context

**HITL because:** the visual UX is a v1 commitment and must be designed via the `/shape` skill before implementation begins. Designs land in `docs/design/onboarding.md` (or equivalent); this slice is the implementation that follows.

## Acceptance criteria

- [ ] All six screens render in the documented sequence
- [ ] Skipping the welcome jumps to language selection
- [ ] Language selection writes choices to Settings (#0006)
- [ ] Triggering a download (#0007) starts in the background and progresses while the interstitial is visible
- [ ] Accessibility permission denial blocks progression with a recovery path (Settings link + try-again)
- [ ] First-Acceptance triggers the Input Monitoring prompt exactly once
- [ ] Onboarding state persists — quitting mid-flow resumes at the same step on next launch
- [ ] Onboarding completed-state survives uninstall and reinstall (or doesn't — to be decided in #0020)
- [ ] Visual implementation matches the approved design from `/shape`

## Blocked by
- #0001, #0006, #0007, #0008
- Design artifact from `/shape`

## User stories addressed
- 11, 12, 13, 14, 21
