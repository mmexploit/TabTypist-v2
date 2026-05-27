# 0019 — Amharic model eval: 50-sentence native-speaker review

**Type:** HITL

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

Per PRD "Further Notes," the Amharic model commit is gated on a native-speaker eval. Build a 50-sentence Amharic eval set spanning news, casual, and formal registers; run TabTypist's Amharic Completer over them; have a native speaker rate Completions blind across (a) EthioNLP Amharic-llama 7B and (b) iocuydi 3.78B. Report results and commit to the v1 Amharic default.

**HITL because:** requires a native Amharic speaker to evaluate output quality. Cannot be automated.

## Acceptance criteria

- [ ] 50-sentence Amharic eval set assembled and committed to `docs/eval/amharic-v1.md`
- [ ] Eval covers at least three registers (news, casual, formal)
- [ ] Both candidate models run over the eval set; outputs captured
- [ ] Native speaker rates each completion blind on a documented scale (e.g., 1–5 on fluency and 1–5 on aptness)
- [ ] Aggregate scores published in `docs/eval/amharic-v1-results.md`
- [ ] Decision recorded as an ADR: v1 Amharic default is committed (or alternatively, escalation triggered if neither model passes the bar)
- [ ] If neither model passes, escalate — do not ship Amharic at v1 in that state

## Blocked by
- #0008

## User stories addressed
Quality gate for stories 2, 3.
