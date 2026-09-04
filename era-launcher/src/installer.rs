use crate::downloads::DownloadManager;
use crate::errors::LauncherError;
use crate::minecraft::java::{JavaInstallation, JavaManager};
use crate::platform::Platform;
use crate::prelude::*;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
const DEFAULT_JAVA_VERSION: u32 = 21;

#[allow(dead_code)]
const ADOPTIUM_API: &str = "https://api.adoptium.net/v3";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)]
pub struct InstallProgress {
    pub step: String,
    pub message: String,
    pub progress: f32,
    pub is_complete: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)]
pub struct InstallResult {
    pub success: bool,
    pub java_installed: bool,
    pub java_path: Option<String>,
    pub launcher_path: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JavaMirror {
    pub name: String,
    pub url: String,
}

pub struct WebInstaller {
    platform: Platform,
    java_version: u32,
    install_dir: PathBuf,
}

impl WebInstaller {
    pub fn new(install_dir: PathBuf, java_version: Option<u32>) -> Result<Self> {
        let platform = Platform::current();
        let java_version = java_version.unwrap_or(DEFAULT_JAVA_VERSION);
        std::fs::create_dir_all(&install_dir)?;
        Ok(Self {
            platform,
            java_version,
            install_dir,
        })
    }

    pub async fn java_mirrors(&self, version: u32) -> Vec<JavaMirror> {
        let mut mirrors = Vec::new();

        if let Some(url) = self.adoptium_jre_url(version).await {
            mirrors.push(JavaMirror {
                name: "Adoptium Temurin".to_string(),
                url,
            });
        }

        mirrors
    }

    async fn adoptium_jre_url(&self, version: u32) -> Option<String> {
        let os = match self.platform.os {
            "windows" => "windows",
            "osx" => "macos",
            _ => "linux",
        };
        let arch = match self.platform.arch {
            "arm64" | "aarch64" => "aarch64",
            _ => "x64",
        };
        let image_type = "jre";

        let api_url = format!(
            "{}/assets/feature_releases/{}/ga?architecture={}&image_type={}&os={}&vendor=eclipse",
            ADOPTIUM_API, version, arch, image_type, os
        );

        let client = reqwest::Client::builder()
            .user_agent("ARGUS-Launcher/0.1")
            .build()
            .ok()?;

        let resp = client.get(&api_url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }

        let json: serde_json::Value = resp.json().await.ok()?;
        let binaries = json.as_array()?.first()?.get("binaries")?.as_array()?;
        let package = binaries.first()?.get("package")?;
        let link = package.get("link")?.as_str()?;

        Some(link.to_string())
    }

    pub fn is_java_installed(&self) -> bool {
        let java_major = JavaManager::required_for_minecraft(&format!("{}.0.0", self.java_version));
        JavaManager::find_compatible(java_major).is_some()
    }

    pub fn detected_java(&self) -> Option<JavaInstallation> {
        let java_major = JavaManager::required_for_minecraft(&format!("{}.0.0", self.java_version));
        JavaManager::find_compatible(java_major)
    }

    pub async fn install_java(
        &self,
        progress_cb: Option<std::sync::Arc<dyn Fn(InstallProgress) + Send + Sync>>,
    ) -> Result<bool> {
        let report = |progress: InstallProgress| {
            if let Some(ref cb) = progress_cb {
                cb(progress);
            }
        };

        if self.is_java_installed() {
            report(InstallProgress {
                step: "java".to_string(),
                message: "Java already installed".to_string(),
                progress: 100.0,
                is_complete: true,
            });
            return Ok(true);
        }

        let java_dir = self
            .install_dir
            .join("runtimes")
            .join(format!("java{}", self.java_version));
        if java_dir.join(get_java_bin_name()).exists() {
            report(InstallProgress {
                step: "java".to_string(),
                message: format!(
                    "Java {} already installed at {}",
                    self.java_version,
                    java_dir.display()
                ),
                progress: 100.0,
                is_complete: true,
            });
            return Ok(true);
        }

        let mirrors = self.java_mirrors(self.java_version).await;

        let mut last_err: Option<LauncherError> = None;
        for mirror in &mirrors {
            report(InstallProgress {
                step: "java".to_string(),
                message: format!(
                    "Downloading Java {} from {}...",
                    self.java_version, mirror.name
                ),
                progress: 0.0,
                is_complete: false,
            });

            std::fs::create_dir_all(&java_dir)?;
            let archive_name = get_archive_name(&mirror.url);
            let archive_path = self.install_dir.join(&archive_name);

            let cb_clone = progress_cb.clone();
            let dm = DownloadManager::new().with_progress_callback(std::sync::Arc::new(move |p| {
                if let Some(ref cb) = cb_clone {
                    let pct = if let Some(total) = p.total_bytes {
                        (p.bytes_downloaded as f32 / total as f32) * 100.0
                    } else {
                        0.0
                    };
                    cb(InstallProgress {
                        step: "java".to_string(),
                        message: format!(
                            "Downloading {} ({} bytes)",
                            p.file_name, p.bytes_downloaded
                        ),
                        progress: pct,
                        is_complete: p.is_complete,
                    });
                }
            }));

            match dm.download(&mirror.url, &archive_path).await {
                Ok(()) => {
                    report(InstallProgress {
                        step: "java".to_string(),
                        message: "Extracting Java...".to_string(),
                        progress: 50.0,
                        is_complete: false,
                    });

                    let extract_result = extract_archive(&archive_path, &java_dir);
                    std::fs::remove_file(&archive_path).ok();

                    match extract_result {
                        Ok(()) => {
                            report(InstallProgress {
                                step: "java".to_string(),
                                message: format!(
                                    "Java {} installed successfully",
                                    self.java_version
                                ),
                                progress: 100.0,
                                is_complete: true,
                            });

                            if verify_java(&java_dir) {
                                return Ok(true);
                            } else {
                                last_err = Some(LauncherError::Java(
                                    "Java verification failed".to_string(),
                                ));
                            }
                        }
                        Err(e) => {
                            last_err = Some(LauncherError::Io(e));
                        }
                    }
                }
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }

        report(InstallProgress {
            step: "java".to_string(),
            message: format!("Java download failed: {:?}", last_err),
            progress: 100.0,
            is_complete: true,
        });

        Ok(false)
    }

    pub fn java_path(&self) -> Option<PathBuf> {
        let java_dir = self
            .install_dir
            .join("runtimes")
            .join(format!("java{}", self.java_version));
        let java_exe = java_dir.join(get_java_bin_name());
        if java_exe.exists() {
            Some(java_exe)
        } else {
            None
        }
    }
}

/// Install a Java runtime if not already present.
/// Returns the path to the installed runtime directory, or an error.
pub async fn install_java_runtime(
    install_dir: PathBuf,
    java_version: u32,
) -> Result<Option<PathBuf>> {
    let installer = WebInstaller::new(install_dir, Some(java_version))?;
    let ok = installer.install_java(None).await?;
    if ok {
        Ok(installer.java_path())
    } else {
        Err(LauncherError::Java("Java download failed".to_string()))
    }
}

fn get_java_bin_name() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("bin").join("javaw.exe")
    } else {
        PathBuf::from("bin").join("java")
    }
}

fn get_archive_name(url: &str) -> String {
    let lower = url.to_lowercase();
    if lower.ends_with(".zip") {
        "java_jre.zip".to_string()
    } else if lower.ends_with(".tar.gz") {
        "java_jre.tar.gz".to_string()
    } else {
        "java_jre.zip".to_string()
    }
}

fn extract_archive(
    archive_path: &Path,
    dest_dir: &Path,
) -> std::result::Result<(), std::io::Error> {
    let lower = archive_path.to_string_lossy().to_lowercase();

    if lower.ends_with(".zip") {
        extract_zip(archive_path, dest_dir)
    } else if lower.ends_with(".tar.gz") {
        extract_tar_gz(archive_path, dest_dir)
    } else {
        extract_zip(archive_path, dest_dir)
    }
}

fn extract_zip(archive_path: &Path, dest_dir: &Path) -> std::result::Result<(), std::io::Error> {
    use std::fs;
    let file = fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    fs::create_dir_all(dest_dir)?;

    let mut top_level: Option<String> = None;
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let file = archive.by_index(i);
        if let Ok(file) = file {
            if let Some(name) = file.enclosed_name() {
                names.push(name.to_path_buf());
            }
        }
    }

    for path in &names {
        if let Some(comp) = path.components().next() {
            let comp_str = comp.as_os_str().to_string_lossy().to_string();
            if top_level.is_none() {
                top_level = Some(comp_str);
            } else if top_level.as_deref() != Some(&comp_str) {
                top_level = None;
                break;
            }
        } else {
            top_level = None;
            break;
        }
    }

    for i in 0..archive.len() {
        let mut zip_file = archive
            .by_index(i)
            .map_err(std::io::Error::other)?;
        let raw_path = match zip_file.enclosed_name() {
            Some(path) => path,
            None => continue,
        };

        let outpath = if let Some(_top) = &top_level {
            let stripped = raw_path
                .components()
                .skip(1)
                .collect::<std::path::PathBuf>();
            if stripped.as_os_str().is_empty() {
                continue;
            }
            dest_dir.join(stripped)
        } else {
            dest_dir.join(raw_path)
        };

        if zip_file.is_dir() {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut outfile = fs::File::create(&outpath)?;
            std::io::copy(&mut zip_file, &mut outfile)?;
        }
    }

    Ok(())
}

fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> std::result::Result<(), std::io::Error> {
    use std::fs;
    let file = fs::File::open(archive_path)?;
    let tar = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(tar);

    fs::create_dir_all(dest_dir)?;

    // Detect a single top-level directory component so we can strip it (the
    // Adoptium JRE tarball ships `jdk-21.0.x/bin/java`, etc.). This matches
    // the zip path so `verify_java` can look in `dest_dir/bin/java`.
    let mut top_level: Option<String> = None;
    for entry in archive.entries().map_err(std::io::Error::other)? {
        let entry = entry.map_err(std::io::Error::other)?;
        let path = entry.path().map_err(std::io::Error::other)?.into_owned();
        if let Some(first) = path.components().next() {
            let comp = first.as_os_str().to_string_lossy().to_string();
            if top_level.is_none() {
                top_level = Some(comp);
            } else if top_level.as_deref() != Some(&comp) {
                top_level = None;
                break;
            }
        } else {
            top_level = None;
            break;
        }
    }

    // Re-open for extraction (entries iterator was consumed above).
    let file = fs::File::open(archive_path)?;
    let tar = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(tar);

    for entry in archive.entries().map_err(std::io::Error::other)? {
        let mut entry = entry.map_err(std::io::Error::other)?;
        let raw_path = entry.path().map_err(std::io::Error::other)?.into_owned();
        let stripped = if let Some(top) = &top_level {
            raw_path
                .strip_prefix(top)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| raw_path.clone())
        } else {
            raw_path.clone()
        };
        if stripped.as_os_str().is_empty() {
            continue;
        }
        let outpath = dest_dir.join(&stripped);
        if let Some(parent) = outpath.parent() {
            fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&outpath)
            .map_err(std::io::Error::other)?;
    }
    Ok(())
}

fn verify_java(java_dir: &Path) -> bool {
    let java_exe = if cfg!(windows) {
        java_dir.join("bin").join("javaw.exe")
    } else {
        java_dir.join("bin").join("java")
    };

    if !java_exe.exists() {
        return false;
    }

    if let Ok(output) = Command::new(&java_exe).arg("-version").output() {
        let output_str = String::from_utf8_lossy(&output.stderr).to_string();
        JavaVersion::parse_output(&java_exe, &output_str).is_some()
    } else {
        false
    }
}

use crate::minecraft::java::JavaVersion;

#[allow(dead_code)]
fn verify_sha256(data: &[u8], expected: &str) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let hash = hex::encode(hasher.finalize());
    hash.eq_ignore_ascii_case(expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_result_fields() {
        let result = InstallResult {
            success: true,
            java_installed: true,
            java_path: Some("/test/path".to_string()),
            launcher_path: Some("/test/path/era-launcher".to_string()),
            error: None,
        };
        assert!(result.success);
        assert!(result.java_installed);
        assert!(result.java_path.is_some());
        assert!(result.launcher_path.is_some());
    }

    #[test]
    fn test_is_newly_installed_java() {
        // Test that java_path() returns None when Java is not installed
        // in the installer's runtime directory
        let install_dir =
            std::env::temp_dir().join(format!("era-test-{}-nonexist", std::process::id()));
        let installer = WebInstaller::new(install_dir.clone(), Some(21)).unwrap();
        // java_path() checks the install directory, not system paths
        assert!(installer.java_path().is_none());
    }

    #[test]
    fn test_java_mirrors_contains_version() {
        let install_dir = std::env::temp_dir().join(format!("era-test-{}", std::process::id()));
        let installer = WebInstaller::new(install_dir.clone(), Some(21)).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mirrors = rt.block_on(installer.java_mirrors(21));
        assert!(!mirrors.is_empty());
        assert!(mirrors.iter().any(|m| m.name.contains("Adoptium")));
    }

    #[test]
    fn test_java_mirrors_structure() {
        let install_dir = std::env::temp_dir().join(format!("era-test-{}", std::process::id()));
        let installer = WebInstaller::new(install_dir, Some(17)).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mirrors = rt.block_on(installer.java_mirrors(17));
        for mirror in &mirrors {
            assert!(!mirror.name.is_empty());
            assert!(mirror.url.starts_with("http"));
            assert!(mirror.url.contains("17"));
        }
    }

    #[test]
    fn test_minecraft_java_versions() {
        assert_eq!(JavaManager::required_for_minecraft("1.16.5"), 8);
        assert_eq!(JavaManager::required_for_minecraft("1.18.2"), 17);
        assert_eq!(JavaManager::required_for_minecraft("1.20.1"), 21);
        assert_eq!(JavaManager::required_for_minecraft("1.21.1"), 21);
    }

    #[test]
    fn test_platform_str() {
        let platform = Platform::current();
        assert!(!platform.os.is_empty());
        assert!(!platform.arch.is_empty());
    }

    #[test]
    fn test_provisioned_runtime_reuse() {
        let install_dir =
            std::env::temp_dir().join(format!("era-test-reuse-{}", std::process::id()));
        let java_dir = install_dir.join("runtimes").join("java21");
        let bin_dir = java_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let java_exe = bin_dir.join(if cfg!(windows) { "javaw.exe" } else { "java" });
        std::fs::write(&java_exe, b"mock").unwrap();

        let installer = WebInstaller::new(install_dir.clone(), Some(21)).unwrap();
        let path = installer.java_path();
        assert!(path.is_some());
        assert_eq!(path.unwrap(), java_exe);

        let _ = std::fs::remove_dir_all(install_dir);
    }

    #[test]
    fn test_install_java_runtime_returns_path_after_setup() {
        let install_dir =
            std::env::temp_dir().join(format!("era-test-install-{}", std::process::id()));
        let java_dir = install_dir.join("runtimes").join("java21");
        let bin_dir = java_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let java_exe = bin_dir.join(if cfg!(windows) { "javaw.exe" } else { "java" });
        std::fs::write(&java_exe, b"mock").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(install_java_runtime(install_dir.clone(), 21));
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.is_some());
        assert_eq!(path.unwrap(), java_exe);

        let _ = std::fs::remove_dir_all(install_dir);
    }

    #[test]
    fn test_adoptium_url_os_branch() {
        // The Adoptium URL path is OS-dependent; assert that each branch
        // produces a valid URL for the running platform and that the API
        // endpoint is well-formed.
        let install_dir = std::env::temp_dir().join(format!("era-test-url-{}", std::process::id()));
        let installer = WebInstaller::new(install_dir.clone(), Some(21)).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let url = rt.block_on(installer.adoptium_jre_url(21));
        if let Some(u) = url {
            assert!(u.starts_with("https://"));
            assert!(u.contains("jdk-21") || u.contains("jre21") || u.contains("/21/"));
        }
        let _ = std::fs::remove_dir_all(install_dir);
    }

    #[test]
    fn test_extract_tar_gz_strips_top_dir() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let tmp = std::env::temp_dir().join(format!("era-test-tar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Synthesize a tar.gz containing a top-level dir `jdk-fake/` and two
        // files inside it.
        let archive_path = tmp.join("jdk.tar.gz");
        let tar_gz = std::fs::File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut tar_builder = tar::Builder::new(enc);
        let entries: [(&str, &[u8]); 2] = [
            ("jdk-fake/bin/java", b"#!/bin/sh\necho fake\n" as &[u8]),
            ("jdk-fake/VERSION", b"21\n" as &[u8]),
        ];
        for (name, body) in entries.iter() {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar_builder.append(&header, *body).unwrap();
        }
        tar_builder.into_inner().unwrap().finish().unwrap();

        let dest = tmp.join("extracted");
        extract_archive(&archive_path, &dest).unwrap();
        assert!(dest.join("bin").join("java").exists());
        assert!(dest.join("VERSION").exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
