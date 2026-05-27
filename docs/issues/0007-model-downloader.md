# 0007 — Model Catalog + Downloader with verification + tests

**Type:** AFK

## Parent PRD
[docs/prd/v1.md](../prd/v1.md)

## What to build

PRD module #5 — Model Catalog & Downloader. Catalog enumerates available models (English default, Amharic default, advanced overrides) with their URL, size, SHA-256, and Ed25519 signature. Downloader fetches with HTTP range requests for resumability, streams to a temp file, verifies checksum and signature before atomic rename into the installed-models directory. Includes module #5 test suite from the PRD.

## Acceptance criteria

- [ ] Catalog is a static list at v1 (no remote update yet) with at least Qwen 2.5 1.5B and the Amharic model entries
- [ ] Download resumes from an interrupted byte offset using Range requests
- [ ] Corrupted bytes (wrong SHA-256) cause the download to fail and the temp file to be deleted
- [ ] Untrusted signature aborts the install before the file is moved into the installed-models directory
- [ ] Progress events emit at a reasonable rate (every ~250ms or every ~1MB) for UI consumption
- [ ] Unit tests cover resume, checksum mismatch, signature failure, and partial-file cleanup

## Blocked by
- #0006

## User stories addressed
- 23, 24, 25
