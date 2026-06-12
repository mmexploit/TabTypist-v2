use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::watch;
use tracing::{debug, info};

const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompletionLength {
    Short,
    Medium,
    #[default]
    Long,
}

impl CompletionLength {
    /// Token budget for each preset.
    pub fn token_budget(self) -> u32 {
        match self {
            CompletionLength::Short  => 11,
            CompletionLength::Medium => 18,
            CompletionLength::Long   => 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub schema_version: u32,

    /// Languages the user has selected (e.g. ["en", "am"])
    pub selected_languages: Vec<String>,

    /// Per-language model overrides: language code → model ID
    pub model_overrides: HashMap<String, String>,

    /// Telemetry opt-in; false by default
    pub telemetry_enabled: bool,

    /// Random install ID for telemetry (UUID string, resettable)
    pub install_id: String,

    /// Per-app exclusions edited by the user (bundle ID → enabled)
    pub app_exclusion_overrides: HashMap<String, bool>,

    /// Bundle IDs of messaging apps where the first-activation toast has been shown
    pub messaging_toast_shown: HashSet<String>,

    /// Onboarding completion state
    pub onboarding_completed: bool,

    /// Phase of onboarding (0 = not started, increments through phases)
    pub onboarding_phase: u8,

    /// Whether Input Monitoring permission was granted
    pub input_monitoring_granted: bool,

    /// Completion length preset; controls how many tokens the core generates.
    #[serde(default)]
    pub completion_length: CompletionLength,

    /// Allow completions to span multiple paragraphs (stops at blank line, not single \n).
    /// Token budget doubles (capped at 60) when enabled.
    #[serde(default)]
    pub multi_line_enabled: bool,

    /// Display name of the user (included in instruct-model personalisation prompts).
    #[serde(default)]
    pub user_name: String,

    /// Free-form writing rules applied globally across all apps.
    #[serde(default)]
    pub custom_rules_global: String,

    /// Per-app writing rules: bundle ID → rule text.
    #[serde(default)]
    pub custom_rules_per_app: HashMap<String, String>,

    /// When true, the current clipboard text is included in the instruct context window.
    #[serde(default)]
    pub clipboard_context_enabled: bool,

    /// HuggingFace API token — required for model downloads (hf.co now needs auth).
    #[serde(default)]
    pub hf_token: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            selected_languages: vec!["en".to_string()],
            model_overrides: HashMap::new(),
            telemetry_enabled: false,
            install_id: new_install_id(),
            app_exclusion_overrides: HashMap::new(),
            messaging_toast_shown: HashSet::new(),
            onboarding_completed: false,
            onboarding_phase: 0,
            input_monitoring_granted: false,
            completion_length: CompletionLength::Long,
            multi_line_enabled: false,
            user_name: String::new(),
            custom_rules_global: String::new(),
            custom_rules_per_app: HashMap::new(),
            clipboard_context_enabled: false,
            hf_token: String::new(),
        }
    }
}

fn new_install_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Simple random ID without external deps: mix of PID + timestamp + random-ish bits
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    format!("{pid:08x}-{ts:032x}")
}

fn migrate(mut s: Settings) -> Settings {
    // Future migrations: match s.schema_version { 0 => { ... s.schema_version = 1; } ... }
    s.schema_version = CURRENT_SCHEMA_VERSION;
    s
}

// ── SettingsStore ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SettingsStore {
    inner: Arc<RwLock<Settings>>,
    path: PathBuf,
    tx: watch::Sender<Settings>,
    #[allow(dead_code)]
    pub rx: watch::Receiver<Settings>,
}

impl SettingsStore {
    /// Load from disk; create defaults if the file doesn't exist.
    pub fn load() -> Result<Self> {
        let path = settings_path()?;
        let settings = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading settings from {path:?}"))?;
            let parsed: Settings = serde_json::from_str(&raw)
                .with_context(|| "parsing settings JSON")?;
            let migrated = migrate(parsed);
            debug!("loaded settings from {path:?}");
            migrated
        } else {
            info!("no settings file found, using defaults");
            Settings::default()
        };

        let (tx, rx) = watch::channel(settings.clone());
        Ok(Self {
            inner: Arc::new(RwLock::new(settings)),
            path,
            tx,
            rx,
        })
    }

    pub fn get(&self) -> Settings {
        self.inner.read().unwrap().clone()
    }

    pub fn update<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut Settings),
    {
        let mut guard = self.inner.write().unwrap();
        f(&mut guard);
        guard.schema_version = CURRENT_SCHEMA_VERSION;
        let new_settings = guard.clone();
        drop(guard);
        self.persist(&new_settings)?;
        let _ = self.tx.send(new_settings);
        Ok(())
    }

    fn persist(&self, settings: &Settings) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("tmp");
        let json = serde_json::to_string_pretty(settings)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        debug!("settings persisted to {:?}", self.path);
        Ok(())
    }

    /// Reset to factory defaults (used by uninstall hygiene and the "Reset TabTypist" action).
    #[allow(dead_code)]
    pub fn reset(&self) -> Result<()> {
        self.update(|s| *s = Settings::default())
    }

    /// Generate a new install ID (privacy: the user can reset telemetry identity).
    #[allow(dead_code)]
    pub fn reset_install_id(&self) -> Result<()> {
        self.update(|s| s.install_id = new_install_id())
    }

    #[allow(dead_code)]
    pub fn watch(&self) -> watch::Receiver<Settings> {
        self.rx.clone()
    }
}

pub fn settings_path() -> Result<PathBuf> {
    let support = dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Library").join("Application Support")))
        .context("could not determine Application Support directory")?;
    Ok(support.join("TabTypist").join("settings.json"))
}

/// Remove all TabTypist data (used on uninstall).
pub fn delete_all_data() -> Result<()> {
    let path = settings_path()?;
    if let Some(dir) = path.parent() {
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
            info!("removed TabTypist data directory at {dir:?}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store_in(dir: &TempDir) -> SettingsStore {
        let path = dir.path().join("settings.json");
        // Write defaults to file so load() finds it
        let settings = Settings::default();
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        // Temporarily override the path by directly constructing the store
        let (tx, rx) = watch::channel(settings.clone());
        SettingsStore {
            inner: Arc::new(RwLock::new(settings)),
            path,
            tx,
            rx,
        }
    }

    #[test]
    fn default_telemetry_off() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        assert!(!store.get().telemetry_enabled, "telemetry must be off by default");
    }

    #[test]
    fn update_persists() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        store.update(|s| s.telemetry_enabled = true).unwrap();
        assert!(store.get().telemetry_enabled);

        // Re-read the file to confirm persistence
        let raw = std::fs::read_to_string(&store.path).unwrap();
        let reloaded: Settings = serde_json::from_str(&raw).unwrap();
        assert!(reloaded.telemetry_enabled);
    }

    #[test]
    fn reset_install_id_changes_id() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let first = store.get().install_id.clone();
        // Sleep a tiny bit so the timestamp changes
        std::thread::sleep(std::time::Duration::from_millis(1));
        store.reset_install_id().unwrap();
        let second = store.get().install_id.clone();
        assert_ne!(first, second, "reset_install_id must generate a new ID");
    }

    #[test]
    fn migration_sets_current_schema_version() {
        let old = Settings {
            schema_version: 0,
            ..Settings::default()
        };
        let migrated = migrate(old);
        assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn completion_length_token_budgets() {
        assert_eq!(CompletionLength::Short.token_budget(),  11);
        assert_eq!(CompletionLength::Medium.token_budget(), 18);
        assert_eq!(CompletionLength::Long.token_budget(),   30);
    }

    #[test]
    fn completion_length_default_is_long() {
        assert_eq!(Settings::default().completion_length, CompletionLength::Long);
    }

    #[test]
    fn multi_line_default_is_off() {
        assert!(!Settings::default().multi_line_enabled);
    }

    #[test]
    fn multi_line_budget_doubling_capped_at_60() {
        use super::CompletionLength;
        let budget = CompletionLength::Long.token_budget(); // 30
        let multi = budget.saturating_mul(2).min(60);
        assert_eq!(multi, 60);
        let over = 40_u32.saturating_mul(2).min(60);
        assert_eq!(over, 60); // capped
    }

    #[test]
    fn completion_length_persists() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        store.update(|s| s.completion_length = CompletionLength::Short).unwrap();
        let raw = std::fs::read_to_string(&store.path).unwrap();
        let reloaded: Settings = serde_json::from_str(&raw).unwrap();
        assert_eq!(reloaded.completion_length, CompletionLength::Short);
    }
}
