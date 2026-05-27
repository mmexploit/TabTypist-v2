---
status: accepted
---

# Default-on in messaging apps (Signal, Telegram, WhatsApp, iMessage)

TabTypist's **Exclusion list** defaults secure-input fields, password managers, and known banking apps to off, but **leaves secure-messaging apps on by default** — the user can disable per-app. The reasoning is that messaging is where users type most casually and where ghost-text completion delivers the most value; defaulting off there would make the product feel broken in its most-used surface. Local-only processing by default and the visible per-app paused affordance are considered sufficient privacy posture for v1.

## Considered options

- **Default-off in all secure-messaging apps.** Safest narrative; loses ~40% of the product's typing-volume value at v1.
- **Default-on with one-time activation toast per messaging app.** Chosen as a v1 mitigation — keeps the default while making the user aware on first activation.
- **Default-on always.** Rejected — gives the user no signal that TabTypist is active in sensitive surfaces.

## Consequences

- In **Hosted mode** specifically, messaging apps are treated as default-off regardless of Local-mode settings. The privacy posture difference between local and hosted is significant enough that the same default cannot apply to both.
- A first-activation toast must ship at v1 for the named messaging apps. It is not optional; without it the default is harder to defend.
- The Exclusion list config is remotely-updatable (signed) so we can move an app from default-on to default-off without an app release if a class of incidents emerges.
