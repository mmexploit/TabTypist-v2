# 0008 — Language Router + Amharic model integration

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

PRD module #3 — Language Router. Pure function: given the prefix text and current user settings, returns the loaded model to route the completion request to. Script detection uses Unicode block ranges (Ge'ez block for Amharic, Latin for English). At this slice, the user can select both English and Amharic, both models load, and switching between scripts in the same field routes correctly.

The Amharic model commit is gated on slice #0019 (eval); this slice integrates whichever Amharic model is currently the candidate (default: EthioNLP/Amharic-llama-base-model 7B 4-bit).

## Acceptance criteria

- [ ] Script detection identifies Ge'ez and Latin from the trailing characters of the prefix
- [ ] Router returns the English model when prefix is Latin-script
- [ ] Router returns the Amharic model when prefix is Ge'ez-script
- [ ] If only one language is enabled in settings, router always returns that model regardless of script
- [ ] Real Amharic completions render in Notes when typing Amharic
- [ ] Switching between English and Amharic mid-document changes which model produces the next completion
- [ ] Unit tests cover the router's verdict matrix

## Blocked by
- #0005, #0007

## User stories addressed
- 2, 3
