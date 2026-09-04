use crate::prelude::*;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownloadProgress {
    pub file_name: String,
    pub bytes_downloaded: usize,
    pub total_bytes: Option<usize>,
    pub is_complete: bool,
}

pub struct DownloadManager {
    client: reqwest::Client,
    on_progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>>,
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            on_progress: None,
        }
    }

    pub fn with_progress_callback(
        mut self,
        cb: Arc<dyn Fn(DownloadProgress) + Send + Sync>,
    ) -> Self {
        self.on_progress = Some(cb);
        self
    }

    pub async fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let temp_dest = dest.with_extension("part");
        let response = self
            .client
            .get(url)
            .header("User-Agent", "EraLauncher/0.1.5")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(LauncherError::Download(format!(
                "HTTP {}",
                response.status()
            )));
        }
        std::fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))?;
        let mut file = std::fs::File::create(&temp_dest)?;
        let mut downloaded: usize = 0;
        let total = response.content_length().map(|t| t as usize);
        let name = dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk)?;
            downloaded += chunk.len();
            if let Some(ref cb) = self.on_progress {
                cb(DownloadProgress {
                    file_name: name.clone(),
                    bytes_downloaded: downloaded,
                    total_bytes: total,
                    is_complete: false,
                });
            }
        }
        std::fs::rename(&temp_dest, dest)?;
        if let Some(ref cb) = self.on_progress {
            cb(DownloadProgress {
                file_name: name,
                bytes_downloaded: downloaded,
                total_bytes: total,
                is_complete: true,
            });
        }
        Ok(())
    }

    pub async fn verify_sha1(&self, path: &Path, expected: &str) -> Result<bool> {
        use sha1::{Digest, Sha1};
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha1::new();
        let mut buffer = [0u8; 8192];
        loop {
            let n = std::io::Read::read(&mut file, &mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        let hash = hex::encode(hasher.finalize());
        Ok(hash.eq_ignore_ascii_case(expected))
    }
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_verify_sha1_correct_hash() {
        let tmp = std::env::temp_dir().join(format!(
            "era-hash-test-{}-{}.txt",
            std::process::id(),
            rand::random::<u64>()
        ));
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(b"hello world").unwrap();
        }
        let dm = DownloadManager::new();
        let expected = "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed";
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(dm.verify_sha1(&tmp, expected)).unwrap();
        assert!(result);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_verify_sha1_incorrect_hash() {
        let tmp = std::env::temp_dir().join(format!(
            "era-hash-test-{}-{}.txt",
            std::process::id(),
            rand::random::<u64>()
        ));
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(b"hello world").unwrap();
        }
        let dm = DownloadManager::new();
        let expected = "0000000000000000000000000000000000000000";
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(dm.verify_sha1(&tmp, expected)).unwrap();
        assert!(!result);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_verify_sha1_empty_file() {
        let tmp = std::env::temp_dir().join(format!(
            "era-hash-test-{}-{}.txt",
            std::process::id(),
            rand::random::<u64>()
        ));
        {
            let _f = std::fs::File::create(&tmp).unwrap();
        }
        let dm = DownloadManager::new();
        let expected = "da39a3ee5e6b4b0d3255bfef95601890afd80709"; // empty string SHA1
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(dm.verify_sha1(&tmp, expected)).unwrap();
        assert!(result);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_download_progress_serialization() {
        let progress = DownloadProgress {
            file_name: "test.jar".to_string(),
            bytes_downloaded: 1024,
            total_bytes: Some(4096),
            is_complete: false,
        };
        let json = serde_json::to_string(&progress).unwrap();
        let deserialized: DownloadProgress = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.file_name, "test.jar");
        assert_eq!(deserialized.bytes_downloaded, 1024);
        assert_eq!(deserialized.total_bytes, Some(4096));
        assert!(!deserialized.is_complete);
    }
}
