---
status: accepted
---

# Hosted mode shape, deferred to v2

**Hosted mode** is the v2 monetization mechanism for TabTypist. It serves two specific user jobs: (a) **substitute for Local mode on hardware that cannot run a capable local model** (the low-end Windows wedge) and (c) **provide higher non-English quality than small local models can deliver** (the Amharic wedge at scale). It explicitly does **not** serve (b) "premium frontier completions for users who could run local but want better quality" — that market is crowded with Cursor, Claude Pro, ChatGPT Plus, and TabTypist has no edge there. The shape is: magic-link email auth, Stripe subscription at ~$3–5/month, generous monthly token cap, silent fallback to Local mode when the cap is hit, no logging or training on user text. V1 ships pure Local mode; this ADR exists so the v2 design is not re-litigated when work begins.

## Service architecture (v2)

- **Proxy a frontier API** (Gemini Flash, Claude Haiku, or similar fast multilingual workhorse) rather than running TabTypist's own inference infrastructure. Building inference ops is its own startup; not appropriate for a small team alongside the product.
- **One model class** chosen for predictable unit economics across both (a) and (c) jobs.
- **No request logging** beyond what is needed to serve the response; explicit "your text is never used to train models" copy in onboarding.

## Auth

- **Magic-link email + long-lived device token in OS keychain.** No passwords, no breach response burden, multi-device works naturally (sign in via email on each device).
- **Stripe Customer Portal** for plan changes, cancellation, payment updates, invoices. Do not build that surface from scratch.

## Pricing

- **One plan, ~$3–5/month.** No free tier in Hosted mode (Local mode is the free tier).
- **Generous monthly token cap (~200K tokens).** When the cap is hit, TabTypist falls back to Local mode silently; if Local mode is unavailable on that machine, show a gentle "you've reached your cap" toast.

## Privacy interaction with Exclusion list

- **Hosted mode treats secure-messaging apps as default-off** even when Local mode would be on for the same app. The privacy posture difference between a local model on the user's machine and a request leaving the device to a third-party API is significant enough that the same default cannot apply to both. (Established in ADR-0002.)
