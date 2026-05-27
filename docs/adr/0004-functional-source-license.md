---
status: accepted
---

# Functional Source License 1.1 (converts to Apache 2.0 after 2 years)

TabTypist is licensed under the **Functional Source License 1.1** with **Apache License 2.0** as the future license and a **2-year conversion delay**. The source is published and auditable, individual users can read/modify/self-build, but **commercial competitive use is restricted during the 2-year window** before each release converts to Apache 2.0. This protects the v2 Hosted-mode monetization runway from cloud-vendoring without permanently locking the code, and preserves the audit-ability that TabTypist's privacy-conscious wedge audience cares about. AGPL was rejected because it constrains relicensing optionality without offering stronger monetization defense than FSL.

## Consequences

- TabTypist **cannot** be marketed as "open source" without a footnote during the restriction window; "source-available" is the accurate term. Apache-converted releases are open source by OSI definition.
- **No CLA required at v1.** Outside contributors hold copyright on their own contributions under FSL terms; the FSL future-license clause covers their code too, so the Apache 2.0 conversion proceeds without per-contributor sign-off. Skipping the CLA preserves contribution velocity at v1; the cost is that TabTypist cannot later *re-restrict* the project (only loosen toward the future license).
- Proprietary additions (Hosted-mode integration UI, premium features) can ship in a closed-source companion build that wraps the FSL core. FSL governs only the code in the open repo.
- The conversion clock runs per-release: each tagged release converts to Apache 2.0 individually after 2 years from its release date, so HEAD is always restricted while older releases progressively open up.
