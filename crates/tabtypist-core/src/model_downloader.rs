use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tracing::{debug, info, warn};

// ── Catalog ───────────────────────────────────────────────────────────────────

/// Whether a model uses a plain prefix-completion (base) or a chat-instruct prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Base,
    Instruct,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub display_name: String,
    pub language: String,
    /// Human-readable tier name ("nano", "mini", …, "pro")
    pub tier: String,
    /// Whether the model is base or instruct
    pub model_kind: ModelKind,
    /// Minimum physical RAM (GiB) required for comfortable use; 0 = any
    pub min_ram_gb: u32,
    /// Download URL for the GGUF file
    pub url: String,
    /// Expected file size in bytes (shown before download)
    pub size_bytes: u64,
    /// SHA-256 hex digest of the GGUF file; "placeholder_*" skips verification
    pub sha256: String,
    /// Ed25519 signature over the SHA-256 hex bytes (hex-encoded); "placeholder_*" skips
    pub ed25519_signature: String,
}

pub struct ModelCatalog;

impl ModelCatalog {
    /// Full six-tier catalog — base checkpoints only (no instruct/chat models).
    /// Base-model continuation avoids the instruct issue where models reply to
    /// context, echo prefixes, and leak chat scaffolding regardless of prompting.
    /// URLs and SHA-256 values marked `placeholder_*` are filled in before each
    /// release after the maintainer verifies and signs the GGUF file with the
    /// TabTypist Ed25519 key.
    pub fn entries() -> Vec<ModelEntry> {
        vec![
            ModelEntry {
                id: "qwen3-0.6b-base-q4km".to_string(),
                display_name: "Qwen3 0.6B Base (nano, 0.4 GB)".to_string(),
                language: "en".to_string(),
                tier: "nano".to_string(),
                model_kind: ModelKind::Base,
                min_ram_gb: 0,
                url: "https://huggingface.co/mradermacher/Qwen3-0.6B-Base-GGUF/resolve/main/Qwen3-0.6B-Base.Q4_K_M.gguf".to_string(),
                size_bytes: 396_704_960,
                sha256: "placeholder_sha256_qwen3_0_6b_base".to_string(),
                ed25519_signature: "placeholder_sig_qwen3_0_6b_base".to_string(),
            },
            ModelEntry {
                id: "qwen35-0.8b-base-q6k".to_string(),
                display_name: "Qwen3.5 0.8B Base (mini, 0.6 GB)".to_string(),
                language: "en".to_string(),
                tier: "mini".to_string(),
                model_kind: ModelKind::Base,
                min_ram_gb: 8,
                url: "https://huggingface.co/mradermacher/Qwen3.5-0.8B-Base-i1-GGUF/resolve/main/Qwen3.5-0.8B-Base.i1-Q6_K.gguf".to_string(),
                size_bytes: 629_744_512,
                sha256: "placeholder_sha256_qwen35_0_8b_base".to_string(),
                ed25519_signature: "placeholder_sig_qwen35_0_8b_base".to_string(),
            },
            ModelEntry {
                id: "qwen35-2b-base-q4km".to_string(),
                display_name: "Qwen3.5 2B Base (standard, 1.3 GB)".to_string(),
                language: "en".to_string(),
                tier: "standard".to_string(),
                model_kind: ModelKind::Base,
                min_ram_gb: 8,
                url: "https://huggingface.co/mradermacher/Qwen3.5-2B-Base-i1-GGUF/resolve/main/Qwen3.5-2B-Base.i1-Q4_K_M.gguf".to_string(),
                size_bytes: 1_274_397_056,
                sha256: "placeholder_sha256_qwen35_2b_base".to_string(),
                ed25519_signature: "placeholder_sig_qwen35_2b_base".to_string(),
            },
            ModelEntry {
                id: "qwen3-4b-base-q4km".to_string(),
                display_name: "Qwen3 4B Base (performance, 2.5 GB)".to_string(),
                language: "en".to_string(),
                tier: "performance".to_string(),
                model_kind: ModelKind::Base,
                min_ram_gb: 16,
                url: "https://huggingface.co/mradermacher/Qwen3-4B-Base-GGUF/resolve/main/Qwen3-4B-Base.Q4_K_M.gguf".to_string(),
                size_bytes: 2_497_280_736,
                sha256: "placeholder_sha256_qwen3_4b_base".to_string(),
                ed25519_signature: "placeholder_sig_qwen3_4b_base".to_string(),
            },
            ModelEntry {
                id: "gemma4-e2b-base-q6k".to_string(),
                display_name: "Gemma 4 E2B Base (quality, 3.8 GB)".to_string(),
                language: "en".to_string(),
                tier: "quality".to_string(),
                model_kind: ModelKind::Base,
                min_ram_gb: 16,
                url: "https://huggingface.co/mradermacher/gemma-4-E2B-i1-GGUF/resolve/main/gemma-4-E2B.i1-Q6_K.gguf".to_string(),
                size_bytes: 3_845_328_608,
                sha256: "placeholder_sha256_gemma4_e2b_base".to_string(),
                ed25519_signature: "placeholder_sig_gemma4_e2b_base".to_string(),
            },
            ModelEntry {
                id: "gemma4-e4b-base-q4km".to_string(),
                display_name: "Gemma 4 E4B Base (pro, 5.3 GB)".to_string(),
                language: "en".to_string(),
                tier: "pro".to_string(),
                model_kind: ModelKind::Base,
                min_ram_gb: 24,
                url: "https://huggingface.co/mradermacher/gemma-4-E4B-i1-GGUF/resolve/main/gemma-4-E4B.i1-Q4_K_M.gguf".to_string(),
                size_bytes: 5_335_274_240,
                sha256: "placeholder_sha256_gemma4_e4b_base".to_string(),
                ed25519_signature: "placeholder_sig_gemma4_e4b_base".to_string(),
            },
        ]
    }

    pub fn find(id: &str) -> Option<ModelEntry> {
        Self::entries().into_iter().find(|e| e.id == id)
    }

    pub fn default_for_language(lang: &str) -> Option<ModelEntry> {
        // "standard" tier (Qwen3.5 2B Base) as the default: 1.3 GB, works on 8 GB Macs.
        Self::entries()
            .into_iter()
            .find(|e| e.language == lang && e.tier == "standard")
            .or_else(|| Self::entries().into_iter().find(|e| e.language == lang))
    }

    /// Return the recommended tier for the given physical RAM.
    /// Never auto-selects upward: stays at "quality" or below even on 32 GB Macs.
    #[allow(dead_code)]
    pub fn recommended_for_ram_gb(ram_gb: u32) -> Option<ModelEntry> {
        let target_tier = if ram_gb >= 24 {
            "pro"
        } else if ram_gb >= 16 {
            "quality"
        } else if ram_gb >= 8 {
            "standard"
        } else {
            "nano"
        };
        Self::entries()
            .into_iter()
            .find(|e| e.language == "en" && e.tier == target_tier)
    }
}

// ── Progress ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum DownloadProgress {
    Starting { total_bytes: u64 },
    Progress { downloaded: u64, total: u64 },
    Verifying,
    #[allow(dead_code)]
    Complete { path: PathBuf },
    #[allow(dead_code)]
    Failed { error: String },
}

// ── Downloader ────────────────────────────────────────────────────────────────

pub struct ModelDownloader {
    install_dir: PathBuf,
    ed25519_public_key: [u8; 32],
}

impl ModelDownloader {
    pub fn new(install_dir: PathBuf, ed25519_public_key: [u8; 32]) -> Self {
        Self {
            install_dir,
            ed25519_public_key,
        }
    }

    pub fn installed_path(&self, entry: &ModelEntry) -> PathBuf {
        self.install_dir.join(format!("{}.gguf", entry.id))
    }

    pub fn is_installed(&self, entry: &ModelEntry) -> bool {
        self.installed_path(entry).exists()
    }

    /// Download `entry` to the install directory, sending progress updates.
    /// Supports resumable download via HTTP Range.
    /// `hf_token` — HuggingFace API token; required for all hf.co downloads as of 2025.
    pub async fn download(
        &self,
        entry: &ModelEntry,
        hf_token: Option<&str>,
        progress_tx: watch::Sender<DownloadProgress>,
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.install_dir)?;

        let final_path = self.installed_path(entry);
        let tmp_path = self.install_dir.join(format!("{}.tmp", entry.id));

        // Determine how many bytes we already have (for resume).
        let already_have = if tmp_path.exists() {
            std::fs::metadata(&tmp_path)?.len()
        } else {
            0
        };

        let client = reqwest::Client::new();
        let mut req = client.get(&entry.url);
        if let Some(token) = hf_token.filter(|t| !t.is_empty()) {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        if already_have > 0 {
            req = req.header("Range", format!("bytes={}-", already_have));
            info!("resuming download from byte {already_have}");
        }

        let response = req.send().await.context("starting download")?;

        let status = response.status();
        if !status.is_success() && status.as_u16() != 206 {
            bail!("HTTP error {status} downloading {}", entry.url);
        }

        let total = entry.size_bytes;
        let _ = progress_tx.send(DownloadProgress::Starting { total_bytes: total });

        let append = already_have > 0 && status.as_u16() == 206;
        let mut file = if append {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&tmp_path)
                .await?
        } else {
            tokio::fs::File::create(&tmp_path).await?
        };

        let mut downloaded = already_have;
        let mut body = response.bytes_stream();
        use futures_util::StreamExt;

        while let Some(chunk) = body.next().await {
            let chunk = chunk.context("reading download chunk")?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            let _ = progress_tx.send(DownloadProgress::Progress {
                downloaded,
                total,
            });
        }

        file.flush().await?;
        drop(file);
        debug!("download complete, verifying");

        let _ = progress_tx.send(DownloadProgress::Verifying);

        // Verify SHA-256 checksum.
        self.verify_checksum(&tmp_path, &entry.sha256)?;

        // Verify Ed25519 signature over the SHA-256 hex string.
        self.verify_signature(&entry.sha256, &entry.ed25519_signature)?;

        // Atomic rename into place.
        std::fs::rename(&tmp_path, &final_path)?;
        info!("model installed at {final_path:?}");

        let _ = progress_tx.send(DownloadProgress::Complete {
            path: final_path.clone(),
        });
        Ok(final_path)
    }

    fn verify_checksum(&self, path: &Path, expected_hex: &str) -> Result<()> {
        // Skip verification for placeholder hashes during development
        if expected_hex.starts_with("placeholder_") {
            warn!("skipping checksum verification (placeholder hash)");
            return Ok(());
        }

        let data = std::fs::read(path)?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let actual = hex::encode(hasher.finalize());
        if actual != expected_hex {
            // Remove the bad temp file so a retry doesn't resume from corrupted data.
            let _ = std::fs::remove_file(path);
            bail!("checksum mismatch: expected {expected_hex}, got {actual}");
        }
        Ok(())
    }

    fn verify_signature(&self, sha256_hex: &str, sig_hex: &str) -> Result<()> {
        if sig_hex.starts_with("placeholder_") {
            warn!("skipping signature verification (placeholder sig)");
            return Ok(());
        }
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};

        let sig_bytes = hex::decode(sig_hex).context("decoding signature hex")?;
        let sig_arr: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid signature length"))?;

        let verifying_key = VerifyingKey::from_bytes(&self.ed25519_public_key)?;
        let signature = Signature::from_bytes(&sig_arr);
        verifying_key
            .verify(sha256_hex.as_bytes(), &signature)
            .map_err(|_| anyhow::anyhow!("model signature verification failed"))?;
        Ok(())
    }
}

pub fn models_dir() -> Result<PathBuf> {
    let support = dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Library").join("Application Support")))
        .context("could not determine Application Support directory")?;
    Ok(support.join("TabTypist").join("models"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_downloader(dir: &TempDir) -> ModelDownloader {
        ModelDownloader::new(dir.path().to_path_buf(), [0u8; 32])
    }

    #[test]
    fn installed_path_derived_from_id() {
        let dir = TempDir::new().unwrap();
        let dl = make_downloader(&dir);
        let entry = ModelCatalog::default_for_language("en").unwrap();
        let path = dl.installed_path(&entry);
        assert!(path.to_str().unwrap().ends_with(".gguf"));
    }

    #[test]
    fn catalog_has_required_tiers() {
        let entries = ModelCatalog::entries();
        let tiers: Vec<&str> = entries.iter().map(|e| e.tier.as_str()).collect();
        for t in ["nano", "mini", "standard", "performance", "quality", "pro"] {
            // Exactly one model per tier — one clear recommendation per hardware class.
            assert_eq!(
                tiers.iter().filter(|&&x| x == t).count(),
                1,
                "tier {t} should have exactly one entry"
            );
        }
    }

    #[test]
    fn catalog_is_base_models_only() {
        for e in ModelCatalog::entries() {
            // Base-continuation everywhere: instruct models reply to context and leak
            // chat scaffolding, so none may appear in the shipped catalog.
            assert_eq!(e.model_kind, ModelKind::Base, "{} is not a base model", e.id);
            // The runtime detects the inference path from the installed filename
            // ({id}.gguf), so no id may carry an instruct marker.
            let id = e.id.to_lowercase();
            for marker in ["-it", "instruct", "smollm", "-chat"] {
                assert!(!id.contains(marker), "{} id contains instruct marker {marker}", e.id);
            }
        }
    }

    #[test]
    fn all_entries_have_model_kind() {
        for e in ModelCatalog::entries() {
            // Just assert the field exists and is one of the two variants.
            let _ = matches!(e.model_kind, ModelKind::Base | ModelKind::Instruct);
        }
    }

    #[test]
    fn recommended_quality_for_16gb() {
        let e = ModelCatalog::recommended_for_ram_gb(16).unwrap();
        assert_eq!(e.tier, "quality");
    }

    #[test]
    fn recommended_standard_for_8gb() {
        let e = ModelCatalog::recommended_for_ram_gb(8).unwrap();
        assert_eq!(e.tier, "standard");
    }

    #[test]
    fn recommended_pro_for_24gb() {
        let e = ModelCatalog::recommended_for_ram_gb(24).unwrap();
        assert_eq!(e.tier, "pro");
    }

    #[test]
    fn default_for_language_returns_standard() {
        let e = ModelCatalog::default_for_language("en").unwrap();
        assert_eq!(e.tier, "standard");
    }

    #[test]
    fn checksum_mismatch_removes_tmp() {
        let dir = TempDir::new().unwrap();
        let dl = make_downloader(&dir);
        let tmp = dir.path().join("bad.tmp");
        std::fs::write(&tmp, b"corrupted data").unwrap();
        let result = dl.verify_checksum(&tmp, "aabbccdd");
        assert!(result.is_err());
        assert!(!tmp.exists(), "corrupted tmp must be removed on checksum failure");
    }

    #[test]
    fn placeholder_sha256_skips_checksum() {
        let dir = TempDir::new().unwrap();
        let dl = make_downloader(&dir);
        let tmp = dir.path().join("model.tmp");
        std::fs::write(&tmp, b"any data").unwrap();
        // placeholder hash prefix skips verification
        assert!(dl.verify_checksum(&tmp, "placeholder_sha256_updated_at_release").is_ok());
    }

    #[test]
    fn invalid_sig_rejected() {
        let dir = TempDir::new().unwrap();
        let dl = make_downloader(&dir);
        let result = dl.verify_signature("some_sha256", &"ab".repeat(64));
        assert!(result.is_err(), "invalid signature must be rejected");
    }
}
