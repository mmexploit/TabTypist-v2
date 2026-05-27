# 0017 — Power-user model override (settings + onboarding "Advanced")

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Expose the **Model override** concept (per CONTEXT.md) in the UI:

- During onboarding, an "Advanced" disclosure under language selection lets the user pick a non-default model per language
- In Settings, a per-language model picker lets users change their choice without reinstalling
- Available alternatives at v1: Qwen 2.5 3B (English higher-quality), iocuydi 3.78B (Amharic low-RAM fallback), plus any future bundled additions

## Acceptance criteria

- [ ] Onboarding "Advanced" disclosure renders without expanding by default
- [ ] Selecting an override writes to Settings and triggers download (#0007) of the alternative
- [ ] Settings panel exposes the same per-language model picker
- [ ] Switching the override at runtime unloads the previous model and loads the new one without an app restart
- [ ] Existing override survives uninstall + reinstall only if the user opts to retain settings (see #0020)
- [ ] Disk-space implication is shown before download confirmation

## Blocked by
- #0007, #0013

## User stories addressed
- 21, 22
