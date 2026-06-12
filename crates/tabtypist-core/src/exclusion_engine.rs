use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// The three-tier verdict returned for a given app/field context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Completions are always off; user cannot override.
    AlwaysOff,
    /// Completions are off by default; user can re-enable.
    DefaultOff { user_enabled: bool },
    /// Completions are on by default; user can disable.
    DefaultOn {
        user_disabled: bool,
        /// First time TabTypist activates here → show the one-time toast.
        show_activation_toast: bool,
    },
}

impl Verdict {
    pub fn completions_active(&self) -> bool {
        match self {
            Verdict::AlwaysOff => false,
            Verdict::DefaultOff { user_enabled } => *user_enabled,
            Verdict::DefaultOn { user_disabled, .. } => !user_disabled,
        }
    }
}

// ── Exclusion list config (can be updated remotely) ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExclusionConfig {
    /// Bundle IDs that are unconditionally off (in addition to the always-off
    /// OS-level secure fields, which are detected per-field, not per-bundle).
    pub always_off_bundles: HashSet<String>,

    /// Bundle IDs that are off by default; user can re-enable in settings.
    pub default_off_bundles: HashSet<String>,

    /// Messaging app bundle IDs: default-on but show first-activation toast.
    pub messaging_bundles: HashSet<String>,
}

impl ExclusionConfig {
    /// The built-in, compiled-in list.  Remote config can replace this.
    pub fn built_in() -> Self {
        Self {
            always_off_bundles: HashSet::new(), // secured via is_secure_field flag
            default_off_bundles: [
                // Terminal emulators (opt-in only — arbitrary shell prompts are risky)
                "com.apple.Terminal",
                "com.googlecode.iterm2",
                "net.kovidgoyal.kitty",
                "org.alacritty",
                "io.alacritty",
                // Password managers
                "com.agilebits.onepassword7",
                "com.agilebits.onepassword-osx",
                "com.bitwarden.desktop",
                "com.bitwarden.browser",
                "com.dashlane.dashlane",
                "com.keepersecurity.KeeperDesktop",
                "com.apple.Passwords",
                // Banking apps (major US/EU)
                "com.chase.bankmobile",
                "com.bankofamerica.BofA-Mobile",
                "com.wellsfargo.WellsFargo",
                "com.citi.citimobile",
                "com.usbank.USBankmobile",
                "com.tdbank.TDMobile",
                "com.capitalone.CapitalOneMobile",
                "com.barclays.barclaysmobilebanking",
                "com.hsbc.hsbc-uk",
                "com.lloydsbank.mobile",
                "com.santander.retail.SantanderMobileUK",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            messaging_bundles: [
                "org.whispersystems.signal-desktop",
                "ru.keepcoder.Telegram",
                "org.telegram.desktop",
                "WhatsApp",
                "com.apple.iChat",
                "com.apple.MobileSMS",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

// ── ExclusionEngine ───────────────────────────────────────────────────────────

pub struct ExclusionEngine {
    config: ExclusionConfig,
}

impl ExclusionEngine {
    pub fn new(config: ExclusionConfig) -> Self {
        Self { config }
    }

    pub fn with_built_in() -> Self {
        Self::new(ExclusionConfig::built_in())
    }

    /// Replace the config with a freshly-fetched (and verified) remote config.
    #[allow(dead_code)]
    pub fn update_config(&mut self, config: ExclusionConfig) {
        self.config = config;
    }

    /// Return the verdict for a given app/field context.
    ///
    /// `is_secure_field` must come from `AXIsUIElementSecure` / `AXIsPasswordField`.
    /// `user_overrides` is the per-bundle-ID map from SettingsStore.
    /// `toast_shown` is the set of bundle IDs where the activation toast has already appeared.
    pub fn verdict(
        &self,
        bundle_id: &str,
        is_secure_field: bool,
        user_overrides: &HashMap<String, bool>,
        toast_shown: &HashSet<String>,
    ) -> Verdict {
        // Secure fields are always-off regardless of everything else.
        if is_secure_field {
            return Verdict::AlwaysOff;
        }

        // Compiled-in always-off bundles.
        if self.config.always_off_bundles.contains(bundle_id) {
            return Verdict::AlwaysOff;
        }

        // Default-off tier (password managers, banking apps).
        if self.config.default_off_bundles.contains(bundle_id) {
            let user_enabled = user_overrides.get(bundle_id).copied().unwrap_or(false);
            return Verdict::DefaultOff { user_enabled };
        }

        // Messaging apps: default-on, but show first-activation toast.
        if self.config.messaging_bundles.contains(bundle_id) {
            let user_disabled = user_overrides.get(bundle_id).copied().unwrap_or(true) == false;
            let show_toast = !toast_shown.contains(bundle_id);
            return Verdict::DefaultOn {
                user_disabled,
                show_activation_toast: show_toast,
            };
        }

        // Everything else: default-on, user can disable.
        let user_disabled = user_overrides.get(bundle_id).copied().unwrap_or(true) == false;
        Verdict::DefaultOn {
            user_disabled,
            show_activation_toast: false,
        }
    }
}

// ── Terminal bundle helpers ───────────────────────────────────────────────────

const TERMINAL_BUNDLES: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "net.kovidgoyal.kitty",
    "org.alacritty",
    "io.alacritty",
];

/// True when the bundle ID belongs to a known terminal emulator.
pub fn is_terminal_bundle(bundle_id: &str) -> bool {
    TERMINAL_BUNDLES.contains(&bundle_id)
}

// ── Remote config verification ────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
pub struct SignedExclusionConfig {
    /// Base64-encoded JSON of ExclusionConfig
    pub payload: String,
    /// Hex-encoded Ed25519 signature over the payload bytes
    pub signature: String,
}

/// Verify and decode a remote exclusion config bundle.
#[allow(dead_code)]
pub fn verify_and_decode(
    bundle: &SignedExclusionConfig,
    public_key_bytes: &[u8; 32],
) -> anyhow::Result<ExclusionConfig> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use ed25519_dalek::{Signature, VerifyingKey};

    let payload_bytes = STANDARD.decode(&bundle.payload)?;
    let sig_bytes = hex::decode(&bundle.signature)?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid signature length"))?;

    let verifying_key = VerifyingKey::from_bytes(public_key_bytes)?;
    let signature = Signature::from_bytes(&sig_arr);

    use ed25519_dalek::Verifier;
    verifying_key
        .verify(&payload_bytes, &signature)
        .map_err(|_| anyhow::anyhow!("exclusion config signature verification failed"))?;

    let config: ExclusionConfig = serde_json::from_slice(&payload_bytes)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_overrides() -> HashMap<String, bool> {
        HashMap::new()
    }

    fn empty_toast() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn secure_field_always_off() {
        let engine = ExclusionEngine::with_built_in();
        let verdict = engine.verdict("com.apple.Notes", true, &empty_overrides(), &empty_toast());
        assert_eq!(verdict, Verdict::AlwaysOff);
        assert!(!verdict.completions_active());
    }

    #[test]
    fn secure_field_overrides_user_enable() {
        let engine = ExclusionEngine::with_built_in();
        let mut overrides = HashMap::new();
        overrides.insert("com.apple.Notes".to_string(), true);
        let verdict = engine.verdict("com.apple.Notes", true, &overrides, &empty_toast());
        assert_eq!(verdict, Verdict::AlwaysOff, "user cannot override secure fields");
    }

    #[test]
    fn password_manager_default_off() {
        let engine = ExclusionEngine::with_built_in();
        let verdict = engine.verdict(
            "com.agilebits.onepassword7",
            false,
            &empty_overrides(),
            &empty_toast(),
        );
        assert!(
            matches!(verdict, Verdict::DefaultOff { user_enabled: false }),
            "1Password must be default-off"
        );
        assert!(!verdict.completions_active());
    }

    #[test]
    fn password_manager_user_can_reenable() {
        let engine = ExclusionEngine::with_built_in();
        let mut overrides = HashMap::new();
        overrides.insert("com.agilebits.onepassword7".to_string(), true);
        let verdict = engine.verdict(
            "com.agilebits.onepassword7",
            false,
            &overrides,
            &empty_toast(),
        );
        assert!(
            matches!(verdict, Verdict::DefaultOff { user_enabled: true }),
            "user override must allow 1Password"
        );
        assert!(verdict.completions_active());
    }

    #[test]
    fn messaging_app_default_on_with_toast() {
        let engine = ExclusionEngine::with_built_in();
        let verdict = engine.verdict(
            "org.whispersystems.signal-desktop",
            false,
            &empty_overrides(),
            &empty_toast(),
        );
        assert!(
            matches!(
                verdict,
                Verdict::DefaultOn {
                    user_disabled: false,
                    show_activation_toast: true
                }
            ),
            "Signal first activation should show toast"
        );
        assert!(verdict.completions_active());
    }

    #[test]
    fn messaging_app_no_toast_after_shown() {
        let engine = ExclusionEngine::with_built_in();
        let mut shown = HashSet::new();
        shown.insert("org.whispersystems.signal-desktop".to_string());
        let verdict = engine.verdict(
            "org.whispersystems.signal-desktop",
            false,
            &empty_overrides(),
            &shown,
        );
        assert!(
            matches!(
                verdict,
                Verdict::DefaultOn {
                    user_disabled: false,
                    show_activation_toast: false
                }
            ),
            "toast must not repeat after it has been shown"
        );
    }

    #[test]
    fn normal_app_default_on() {
        let engine = ExclusionEngine::with_built_in();
        let verdict = engine.verdict("com.apple.Notes", false, &empty_overrides(), &empty_toast());
        assert!(
            matches!(
                verdict,
                Verdict::DefaultOn {
                    user_disabled: false,
                    show_activation_toast: false
                }
            )
        );
        assert!(verdict.completions_active());
    }

    #[test]
    fn normal_app_user_can_disable() {
        let engine = ExclusionEngine::with_built_in();
        let mut overrides = HashMap::new();
        overrides.insert("com.apple.Notes".to_string(), false);
        let verdict = engine.verdict("com.apple.Notes", false, &overrides, &empty_toast());
        assert!(!verdict.completions_active());
    }

    #[test]
    fn tampered_remote_config_rejected() {
        use base64::Engine;
        // A zero key will reject any real signature
        let zero_key = [0u8; 32];
        let bundle = SignedExclusionConfig {
            payload: base64::engine::general_purpose::STANDARD
                .encode(b"{\"always_off_bundles\":[],\"default_off_bundles\":[],\"messaging_bundles\":[]}"),
            signature: "0".repeat(128),
        };
        let result = verify_and_decode(&bundle, &zero_key);
        assert!(result.is_err(), "tampered/unsigned config must be rejected");
    }
}
