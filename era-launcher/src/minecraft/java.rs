use crate::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JavaVersion {
    pub major: u32,
    pub minor: u32,
    pub path: PathBuf,
}

impl JavaVersion {
    pub fn parse_output(path: &Path, output: &str) -> Option<Self> {
        let version = output.lines().find(|l| l.contains("version"))?;
        let num_str = version
            .chars()
            .filter(|c| c.is_numeric() || *c == '.')
            .collect::<String>();
        let parts: Vec<&str> = num_str.split('.').collect();
        let major = parts.first()?.parse().ok()?;
        let minor = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        let major = if major == 1 { minor } else { major };
        Some(Self {
            major,
            minor: 0,
            path: path.to_path_buf(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JavaInstallation {
    pub path: PathBuf,
    pub version: Option<JavaVersion>,
}

pub struct JavaManager;

impl JavaManager {
    pub fn detect_all() -> Vec<JavaInstallation> {
        let mut installs = Vec::new();
        let candidates = Self::candidate_paths();
        for path in candidates {
            if let Ok(output) = Self::run_java_version(&path) {
                if let Some(version) = JavaVersion::parse_output(&path, &output) {
                    installs.push(JavaInstallation {
                        path,
                        version: Some(version),
                    });
                } else {
                    installs.push(JavaInstallation {
                        path,
                        version: None,
                    });
                }
            }
        }

        let managed = Self::managed_candidate_paths();
        for path in managed {
            if let Ok(output) = Self::run_java_version(&path) {
                if let Some(version) = JavaVersion::parse_output(&path, &output) {
                    installs.push(JavaInstallation {
                        path,
                        version: Some(version),
                    });
                } else {
                    installs.push(JavaInstallation {
                        path,
                        version: None,
                    });
                }
            }
        }

        installs
    }

    pub fn detect_compatible() -> Vec<JavaInstallation> {
        Self::detect_all()
            .into_iter()
            .filter(|i| i.version.as_ref().map(|v| v.major >= 17).unwrap_or(false))
            .collect()
    }

    pub fn cleanup_old_managed_javas(min_major: u32) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        let base = if cfg!(windows) {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|p| p.join("EraLauncher").join("runtimes"))
        } else {
            std::env::var_os("HOME").map(|h| {
                PathBuf::from(h)
                    .join(".local")
                    .join("share")
                    .join("EraLauncher")
                    .join("runtimes")
            })
        };
        if let Some(base) = base {
            if let Ok(entries) = std::fs::read_dir(&base) {
                for entry in entries.flatten() {
                    let java_exe = entry.path().join("bin").join(if cfg!(windows) {
                        "java.exe"
                    } else {
                        "java"
                    });
                    if !java_exe.exists() {
                        continue;
                    }
                    if let Ok(output) = Self::run_java_version(&java_exe) {
                        if let Some(version) = JavaVersion::parse_output(&java_exe, &output) {
                            if version.major < min_major {
                                let _ = std::fs::remove_dir_all(entry.path());
                                removed.push(entry.path().to_string_lossy().to_string());
                            }
                        }
                    }
                }
            }
        }
        Ok(removed)
    }

    pub fn find_compatible(required_major: u32) -> Option<JavaInstallation> {
        let installs = Self::detect_all();
        installs
            .into_iter()
            .filter(|i| i.version.as_ref().map(|v| v.major) >= Some(required_major))
            .max_by_key(|i| i.version.as_ref().map(|v| v.major))
    }

    pub fn required_for_minecraft(version: &str) -> u32 {
        let parts: Vec<&str> = version.split('.').collect();
        let major = parts
            .first()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(1);
        if major == 1 {
            let minor = parts
                .get(1)
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            match minor {
                0..=7 => 8,
                8..=12 => 8,
                13..=15 => 8,
                16 => 8,
                17..=18 => 17,
                19..=21 => 21,
                22 => 21,
                _ => 21,
            }
        } else {
            match major {
                25.. => 25,
                24 => 21,
                21 => 21,
                17..=20 => 21,
                _ => 8,
            }
        }
    }

    fn candidate_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let bin_name = if cfg!(windows) { "java.exe" } else { "java" };

        if let Some(java_home) = std::env::var_os("JAVA_HOME") {
            let p = PathBuf::from(java_home).join("bin").join(bin_name);
            paths.push(p);
        }

        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                paths.push(dir.join(bin_name));
            }
        }

        if cfg!(windows) {
            let program_files = std::env::var("ProgramFiles").unwrap_or_default();
            let program_files_x86 = std::env::var("ProgramFiles(x86)").unwrap_or_default();
            let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();

            for base in [program_files, program_files_x86, local_app_data] {
                if base.is_empty() {
                    continue;
                }
                if let Ok(entries) = std::fs::read_dir(base) {
                    for entry in entries.flatten() {
                        let p = entry.path().join("bin").join(bin_name);
                        paths.push(p);
                    }
                }
            }
        } else {
            // Linux/macOS: walk the standard JVM install roots that Debian,
            // Ubuntu, Fedora, and Homebrew use.
            for base in ["/usr/lib/jvm", "/usr/lib64/jvm", "/opt/homebrew/opt", "/opt/local"] {
                if let Ok(entries) = std::fs::read_dir(base) {
                    for entry in entries.flatten() {
                        let p = entry.path().join("bin").join(bin_name);
                        paths.push(p);
                    }
                }
            }
        }

        paths
    }

    fn managed_candidate_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let bin_name = if cfg!(windows) { "javaw.exe" } else { "java" };

        // Windows uses LOCALAPPDATA, XDG-compliant Linux/macOS use
        // ~/.local/share. Both should agree with what `installer::install_java`
        // writes, so detection stays in lock-step with provisioning.
        let base = if cfg!(windows) {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|p| p.join("EraLauncher").join("runtimes"))
        } else {
            std::env::var_os("HOME").map(|h| {
                PathBuf::from(h)
                    .join(".local")
                    .join("share")
                    .join("EraLauncher")
                    .join("runtimes")
            })
        };
        if let Some(base) = base {
            if let Ok(entries) = std::fs::read_dir(base) {
                for entry in entries.flatten() {
                    let path = entry.path().join("bin").join(bin_name);
                    paths.push(path);
                }
            }
        }

        paths
    }

    fn run_java_version(path: &Path) -> Result<String> {
        let output = Command::new(path)
            .arg("-version")
            .output()
            .map_err(|e| LauncherError::Process(format!("Failed to run java: {}", e)))?;
        let text = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_required_for_minecraft_1_8() {
        assert_eq!(JavaManager::required_for_minecraft("1.8.9"), 8);
    }

    #[test]
    fn test_required_for_minecraft_1_16() {
        assert_eq!(JavaManager::required_for_minecraft("1.16.2"), 8);
    }

    #[test]
    fn test_required_for_minecraft_1_17() {
        assert_eq!(JavaManager::required_for_minecraft("1.17.1"), 17);
    }

    #[test]
    fn test_required_for_minecraft_1_20() {
        assert_eq!(JavaManager::required_for_minecraft("1.20.1"), 21);
    }

    #[test]
    fn test_required_for_minecraft_1_21() {
        assert_eq!(JavaManager::required_for_minecraft("1.21.1"), 21);
    }

    #[test]
    fn test_required_for_minecraft_1_12() {
        assert_eq!(JavaManager::required_for_minecraft("1.12.2"), 8);
    }

    #[test]
    fn test_required_for_invalid_version() {
        assert_eq!(JavaManager::required_for_minecraft("invalid"), 8);
    }

    #[test]
    fn test_required_for_empty_version() {
        assert_eq!(JavaManager::required_for_minecraft(""), 8);
    }

    #[test]
    fn test_required_for_minecraft_24_year() {
        assert_eq!(JavaManager::required_for_minecraft("24.0"), 21);
    }

    #[test]
    fn test_required_for_minecraft_25_year() {
        assert_eq!(JavaManager::required_for_minecraft("25.1"), 25);
    }

    #[test]
    fn test_required_for_minecraft_26_year() {
        assert_eq!(JavaManager::required_for_minecraft("26.2"), 25);
    }

    #[test]
    fn test_required_for_minecraft_26_year_no_patch() {
        assert_eq!(JavaManager::required_for_minecraft("26"), 25);
    }

    #[test]
    fn test_parse_output_java_8() {
        let output =
            "openjdk version \"1.8.0_331\"\nOpenJDK Runtime Environment (build 1.8.0_331-b09)\n";
        let result = JavaVersion::parse_output(Path::new("/usr/bin/java"), output);
        assert!(result.is_some());
        let v = result.unwrap();
        assert_eq!(v.major, 8);
    }

    #[test]
    fn test_parse_output_java_17() {
        let output =
            "openjdk version \"17.0.2\" 2022-01-18\nOpenJDK Runtime Environment (build 17.0.2+8)\n";
        let result = JavaVersion::parse_output(Path::new("/usr/bin/java"), output);
        assert!(result.is_some());
        let v = result.unwrap();
        assert_eq!(v.major, 17);
    }

    #[test]
    fn test_parse_output_java_21() {
        let output =
            "openjdk version \"21.0.2\" 2023-10-17\nOpenJDK Runtime Environment (build 21.0.2+7)\n";
        let result = JavaVersion::parse_output(Path::new("/usr/bin/java"), output);
        assert!(result.is_some());
        let v = result.unwrap();
        assert_eq!(v.major, 21);
    }

    #[test]
    fn test_parse_output_invalid() {
        let output = "some random text without version";
        let result = JavaVersion::parse_output(Path::new("/usr/bin/java"), output);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_output_empty() {
        let result = JavaVersion::parse_output(Path::new("/usr/bin/java"), "");
        assert!(result.is_none());
    }

    #[test]
    fn test_managed_candidate_paths_uses_localappdata() {
        let temp = std::env::temp_dir().join(format!("era-test-managed-{}", std::process::id()));
        let bin = if cfg!(windows) {
            temp.join("EraLauncher")
                .join("runtimes")
                .join("java21")
                .join("bin")
        } else {
            temp.join(".local")
                .join("share")
                .join("EraLauncher")
                .join("runtimes")
                .join("java21")
                .join("bin")
        };
        let _ = std::fs::create_dir_all(&bin);
        if cfg!(windows) {
            unsafe {
                std::env::set_var("LOCALAPPDATA", &temp);
            }
        } else {
            unsafe {
                std::env::set_var("HOME", &temp);
            }
        }
        let paths = JavaManager::managed_candidate_paths();
        assert!(paths.len() >= 1);
        let expected = bin.join(if cfg!(windows) { "javaw.exe" } else { "java" });
        assert!(paths.contains(&expected));
        if cfg!(windows) {
            unsafe {
                std::env::remove_var("LOCALAPPDATA");
            }
        } else {
            unsafe {
                std::env::remove_var("HOME");
            }
        }
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn test_managed_candidate_paths_ignores_empty_localappdata() {
        if cfg!(windows) {
            unsafe {
                std::env::remove_var("LOCALAPPDATA");
            }
        } else {
            unsafe {
                std::env::remove_var("HOME");
            }
        }
        let paths = JavaManager::managed_candidate_paths();
        assert!(paths.is_empty());
    }

    #[test]
    fn test_managed_candidate_paths_skips_missing_directory() {
        let temp =
            std::env::temp_dir().join(format!("era-test-managed-missing-{}", std::process::id()));
        if cfg!(windows) {
            unsafe {
                std::env::set_var("LOCALAPPDATA", &temp);
            }
        } else {
            unsafe {
                std::env::set_var("HOME", &temp);
            }
        }
        let paths = JavaManager::managed_candidate_paths();
        assert!(paths.is_empty());
        if cfg!(windows) {
            unsafe {
                std::env::remove_var("LOCALAPPDATA");
            }
        } else {
            unsafe {
                std::env::remove_var("HOME");
            }
        }
        let _ = std::fs::remove_dir_all(temp);
    }
}
