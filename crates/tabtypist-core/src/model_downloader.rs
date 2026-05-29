use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tracing::{debug, info, warn};

// ── Catalog ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub display_name: String,
    pub language: String,
    /// Download URL for the GGUF file
    pub url: String,
    /// Expected file size in bytes (shown before download)
    pub size_bytes: u64,
    /// SHA-256 hex digest of the GGUF file
    pub sha256: String,
    /// Ed25519 signature over the SHA-256 hex bytes (hex-encoded)
    pub ed25519_signature: String,
}

pub struct ModelCatalog;

impl ModelCatalog {
    pub fn entries() -> Vec<ModelEntry> {
        vec![
            ModelEntry {
                id: "qwen2.5-1.5b-base-q4".to_string(),
                display_name: "Qwen 2.5 1.5B (English, ~900 MB)".to_string(),
                language: "en".to_string(),
                // Base (non-instruct) model — better text continuation quality.
                // Qwen publishes no official base GGUF; this is a community repackage.
                // The maintainer verifies the file, signs with TabTypist's Ed25519 key,
                // and may rehost before release. SHA-256 + signature are updated at that time.
                url: "https://huggingface.co/neopolita/qwen2.5-1.5b-gguf/resolve/main/qwen2.5-1.5b_q4_k_m.gguf".to_string(),
                size_bytes: 986_000_000,
                sha256: "placeholder_sha256_updated_at_release".to_string(),
                ed25519_signature: "placeholder_sig_updated_at_release".to_string(),
            },
        ]
    }

    pub fn find(id: &str) -> Option<ModelEntry> {
        Self::entries().into_iter().find(|e| e.id == id)
    }

    pub fn default_for_language(lang: &str) -> Option<ModelEntry> {
        Self::entries().into_iter().find(|e| e.language == lang)
    }
}

// ── Progress ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum DownloadProgress {
    Starting { total_bytes: u64 },
    Progress { downloaded: u64, total: u64 },
    Verifying,
    Complete { path: PathBuf },
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
    pub async fn download(
        &self,
        entry: &ModelEntry,
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
        use tokio_util::io::StreamReader;
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
        use base64::{engine::general_purpose::STANDARD, Engine};
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
