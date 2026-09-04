use crate::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub id: String,
    pub loader_type: String,
    pub main_class: Option<String>,
    pub java_version: Option<u32>,
    pub has_client_jar: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub path: PathBuf,
    pub exists: bool,
    pub loader_type: String,
    pub versions: Vec<VersionInfo>,
    pub library_count: usize,
    pub asset_indexes: Vec<String>,
    pub asset_object_count: usize,
    pub mods: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Default)]
pub struct SystemScanner;

impl SystemScanner {
    pub fn new() -> Self {
        Self
    }

    pub fn scan(&self) -> Result<Vec<ScanResult>> {
        let mut results = Vec::new();
        let candidates = self.candidate_dirs();
        for dir in candidates {
            let exists = dir.exists();
            let mut result = ScanResult {
                path: dir.clone(),
                exists,
                loader_type: String::new(),
                versions: Vec::new(),
                library_count: 0,
                asset_indexes: Vec::new(),
                asset_object_count: 0,
                mods: Vec::new(),
                notes: Vec::new(),
            };
            if exists {
                self.scan_directory(&dir, &mut result);
            }
            results.push(result);
        }
        Ok(results)
    }

    fn candidate_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if cfg!(windows) {
            if let Ok(appdata) = std::env::var("APPDATA") {
                dirs.push(PathBuf::from(appdata).join(".minecraft"));
            }
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                dirs.push(PathBuf::from(local).join(".minecraft"));
            }
        } else if cfg!(target_os = "macos") {
            if let Some(home) = dirs::home_dir() {
                dirs.push(home.join("Library/Application Support/minecraft"));
            }
        } else if cfg!(target_os = "linux") {
            if let Some(home) = dirs::home_dir() {
                dirs.push(home.join(".minecraft"));
            }
        }
        dirs
    }

    fn scan_directory(&self, dir: &Path, result: &mut ScanResult) {
        let versions_dir = dir.join("versions");
        if versions_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&versions_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let id = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_string();
                        let json_path = path.join(format!("{}.json", id));
                        if json_path.exists() {
                            if let Ok(content) = std::fs::read_to_string(&json_path) {
                                if let Ok(info) = Self::parse_version_json(&content) {
                                    result.versions.push(info);
                                }
                            }
                        }
                    }
                }
            }
        }

        let libraries_dir = dir.join("libraries");
        if libraries_dir.exists() {
            result.library_count = walkdir::WalkDir::new(&libraries_dir)
                .into_iter()
                .filter(|e| e.as_ref().map(|e| e.path().is_file()).unwrap_or(false))
                .count();
        }

        let assets_dir = dir.join("assets");
        if assets_dir.exists() {
            let objects = assets_dir.join("objects");
            if objects.exists() {
                result.asset_object_count = walkdir::WalkDir::new(&objects)
                    .into_iter()
                    .filter(|e| e.as_ref().map(|e| e.path().is_file()).unwrap_or(false))
                    .count();
            }
            let indexes = assets_dir.join("indexes");
            if indexes.exists() {
                if let Ok(entries) = std::fs::read_dir(&indexes) {
                    for entry in entries.flatten() {
                        if let Some(name) = entry.path().file_stem().and_then(|n| n.to_str()) {
                            result.asset_indexes.push(name.to_string());
                        }
                    }
                }
            }
        }

        let mods_dir = dir.join("mods");
        if mods_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&mods_dir) {
                for entry in entries.flatten() {
                    if let Some(name) = entry.path().file_name().and_then(|n| n.to_str()) {
                        result.mods.push(name.to_string());
                    }
                }
            }
        }

        if !result.versions.is_empty() {
            result.loader_type = "scanned".to_string();
        }
    }

    pub fn cleanup_foreign_assets(dir: &Path) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        if !dir.exists() {
            return Ok(removed);
        }

        let foreign_markers = [
            "launcher_accounts.json",
            "launcher_profiles.json",
            "launcher_logs",
            "tlauncher",
            "minecraft-launcher",
            "HMCL",
            "hmcl",
            "PrismLauncher",
            "prismlauncher",
            "ATLauncher",
            "atlauncher",
            "MultiMC",
            "multimc",
            ".fabric",
            ".quilt",
            "modrinth",
            "curseforge",
            "Curse",
            " curseforge",
        ];

        for entry in walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file() || e.path().is_dir())
        {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                for marker in &foreign_markers {
                    if name.contains(marker) {
                        let _ = std::fs::remove_file(path);
                        let _ = std::fs::remove_dir_all(path);
                        removed.push(path.to_string_lossy().to_string());
                        break;
                    }
                }
            }
        }

        Ok(removed)
    }

    fn parse_version_json(content: &str) -> Result<VersionInfo> {
        let v: serde_json::Value = serde_json::from_str(content)?;
        let id = v
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let main_class = v
            .get("mainClass")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let java_version = v
            .get("javaVersion")
            .and_then(|j| j.get("major"))
            .and_then(|m| m.as_u64())
            .map(|m| m as u32);
        let has_client_jar = v.get("downloads").and_then(|d| d.get("client")).is_some();
        Ok(VersionInfo {
            id,
            loader_type: "vanilla".to_string(),
            main_class,
            java_version,
            has_client_jar,
        })
    }
}
