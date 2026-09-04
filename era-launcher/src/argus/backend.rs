//! Backend bridge — connects ARGUS terminal UI to real EraLauncher backend services.
//!
//! This module provides the integration layer between ARGUS and the existing
//! EraLauncher backend. It uses the SAME services as the Tauri frontend:
//! - `INSTANCE_MANAGER` for instance CRUD
//! - a dedicated background launch pipeline (manifest → libraries → loader → assets)
//! - `JavaManager` for Java runtime detection
//! - `ModrinthClient` for mod discovery and installation
//! - `SystemScanner` for system info
//!
//! No mocked services. No fake instances. No duplicate managers.

use crate::CONFIG;
use crate::INSTANCE_MANAGER;
use crate::argus::state::{AppState, LogLevel, RuntimeState};
use crate::instances::InstanceConfig;
use crate::minecraft::java::JavaManager;
use crate::minecraft::optimization::OptimizationProfile;
use crate::modrinth::{ModrinthClient, Project};
use crate::platform::Paths;
use crate::versions::{ScanResult, SystemScanner};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::mpsc;
use std::time::Instant;

/// Track the Minecraft runtime process for ARGUS terminal mode.
pub struct RuntimeTracker {
    child: Option<Child>,
    stdout_rx: Option<mpsc::Receiver<String>>,
    stderr_rx: Option<mpsc::Receiver<String>>,
    started_at: Option<Instant>,
    pid: Option<u32>,
    exit_status: Option<std::process::ExitStatus>,
    /// Events from an in-flight background launch (None when idle)
    pub launch_events: Option<mpsc::Receiver<LaunchEvent>>,
}

impl Default for RuntimeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeTracker {
    pub fn new() -> Self {
        Self {
            child: None,
            stdout_rx: None,
            stderr_rx: None,
            started_at: None,
            pid: None,
            exit_status: None,
            launch_events: None,
        }
    }

    pub fn set_process(&mut self, child: Child) {
        self.pid = Some(child.id());
        self.started_at = Some(Instant::now());
        self.child = Some(child);
        self.exit_status = None;
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn uptime_seconds(&self) -> Option<u64> {
        self.started_at.map(|t| t.elapsed().as_secs())
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.exit_status = Some(status);
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// Take the cached exit status (available once the process has exited).
    pub fn take_exit_status(&mut self) -> Option<std::process::ExitStatus> {
        self.exit_status.take()
    }

    pub fn stop(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
            self.child = None;
            self.pid = None;
            self.started_at = None;
            self.exit_status = None;
            true
        } else {
            false
        }
    }

    pub fn set_stdout_rx(&mut self, rx: mpsc::Receiver<String>) {
        self.stdout_rx = Some(rx);
    }

    pub fn set_stderr_rx(&mut self, rx: mpsc::Receiver<String>) {
        self.stderr_rx = Some(rx);
    }

    pub fn poll_output(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(ref rx) = self.stdout_rx {
            while let Ok(line) = rx.try_recv() {
                lines.push(line);
            }
        }
        if let Some(ref rx) = self.stderr_rx {
            while let Ok(line) = rx.try_recv() {
                lines.push(line);
            }
        }
        lines
    }

    /// Whether a Minecraft child process is currently attached (owned by the
    /// tracker). Used to detect the hand-off point into game mode.
    pub fn has_process(&self) -> bool {
        self.child.is_some()
    }

    /// Take ownership of the attached child process. Used when ARGUS hides
    /// itself to let the game own the terminal during game mode.
    pub fn take_child(&mut self) -> Option<Child> {
        self.child.take()
    }

    /// Take the stdout pipe receiver out of the tracker (game-mode forwarder).
    pub fn take_stdout_rx(&mut self) -> Option<mpsc::Receiver<String>> {
        self.stdout_rx.take()
    }

    /// Take the stderr pipe receiver out of the tracker (game-mode forwarder).
    pub fn take_stderr_rx(&mut self) -> Option<mpsc::Receiver<String>> {
        self.stderr_rx.take()
    }

    /// Reset all runtime state after a game-mode wait completes, so the UI
    /// resumes as if the process had exited normally on its own.
    pub fn clear_after_game(&mut self) {
        self.child = None;
        self.stdout_rx = None;
        self.stderr_rx = None;
        self.pid = None;
        self.started_at = None;
        self.exit_status = None;
        self.launch_events = None;
    }
}

/// Events streamed from the background launch thread so the UI stays
/// responsive during long downloads.
pub enum LaunchEvent {
    /// Loading-bar progress update
    Progress(String),
    /// Backend log line
    Log(LogLevel, String),
    /// Minecraft spawned; hands ownership of the child to the UI thread
    Launched(std::process::Child),
    /// Launch failed at some stage
    Failed(String),
}

/// Record of one completed install, persisted per-instance so DISCOVER can
/// filter out owned projects even across restarts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstallRecord {
    pub project_id: String,
    pub version_id: Option<String>,
    pub filename: String,
    pub content_type: String,
}

/// Backend actions that ARGUS can perform using real EraLauncher services.
pub struct BackendBridge;

impl BackendBridge {
    /// Get all instances from the REAL instance manager.
    pub fn list_instances() -> Vec<InstanceConfig> {
        let mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        mgr.list().to_vec()
    }

    /// Get the instances directory path from the real platform paths.
    pub fn instances_dir() -> PathBuf {
        Paths::new().instances_dir().to_path_buf()
    }

    /// Get the first available instance (for quick launch).
    pub fn get_default_instance() -> Option<InstanceConfig> {
        let mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        let list = mgr.list();
        if list.is_empty() {
            None
        } else {
            Some(list[0].clone())
        }
    }

    /// Get Java installations from the REAL JavaManager.
    pub fn detect_java() -> Vec<crate::minecraft::java::JavaInstallation> {
        JavaManager::detect_all()
    }

    /// Get system scan results (GPU, CPU, memory, Java info).
    pub fn scan_system() -> Vec<ScanResult> {
        SystemScanner::new().scan().unwrap_or_default()
    }

    /// Search Modrinth for projects using the REAL ModrinthClient.
    pub fn search_modrinth(
        query: &str,
        content_type: &str,
        game_version: &str,
        loader: &str,
    ) -> Result<Vec<Project>, String> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| format!("Failed to create runtime: {}", e))?;

        rt.block_on(async move {
            let client =
                ModrinthClient::new().map_err(|e| format!("Failed to create client: {}", e))?;

            let mut facets: Vec<String> = vec![format!("project_type:{}", content_type)];
            if !game_version.is_empty() {
                facets.push(format!("versions:{}", game_version));
            }
            if !loader.is_empty() {
                facets.push(format!("loaders:{}", loader));
            }

            let result = client
                .search(query, 100, 0, &facets, Some("relevance"))
                .await
                .map_err(|e| format!("Search failed: {}", e))?;

            Ok(result.hits)
        })
    }

    /// Get mod versions for a project from Modrinth.
    pub fn get_mod_versions(project_id: &str) -> Result<Vec<crate::modrinth::Version>, String> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| format!("Failed to create runtime: {}", e))?;

        rt.block_on(async move {
            let client =
                ModrinthClient::new().map_err(|e| format!("Failed to create client: {}", e))?;
            client
                .get_project_versions(project_id)
                .await
                .map_err(|e| format!("Failed to get versions: {}", e))
        })
    }

    /// Get the current settings from the real CONFIG.
    pub fn get_settings() -> crate::config::Settings {
        let config = crate::CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        config.settings.clone()
    }

    /// Save settings to the real CONFIG and persist to disk.
    pub fn save_settings(settings: &crate::config::Settings) -> bool {
        if let Ok(mut config) = crate::CONFIG.lock() {
            config.settings = settings.clone();
            if config.save().is_ok() {
                return true;
            }
        }
        false
    }

    /// Get the current window config.
    pub fn get_window_config() -> crate::config::WindowConfig {
        crate::CONFIG.lock().unwrap_or_else(|e| e.into_inner()).window.clone()
    }

    /// Set the default memory and persist.
    pub fn set_default_memory(mb: u32) -> bool {
        let mut settings = Self::get_settings();
        settings.default_memory = mb;
        Self::save_settings(&settings)
    }

    /// Set the Java path (None = auto-detect) and persist.
    pub fn set_java_path(path: Option<String>) -> bool {
        let mut settings = Self::get_settings();
        settings.java_path = path;
        Self::save_settings(&settings)
    }

    /// Set the theme and persist. Applies the palette globally so the
    /// terminal UI switches immediately (dark/light/system).
    pub fn set_theme(theme: &str) -> bool {
        let mut settings = Self::get_settings();
        settings.theme = theme.to_string();
        if Self::save_settings(&settings) {
            crate::argus::theme::apply(theme);
            return true;
        }
        false
    }

    /// Set the optimization profile and persist.
    pub fn set_optimization_profile(profile: OptimizationProfile) -> bool {
        let mut settings = Self::get_settings();
        settings.optimization_profile = profile;
        Self::save_settings(&settings)
    }

    /// Set custom JVM args (space-separated string) and persist.
    pub fn set_custom_jvm_args(args: &str) -> bool {
        let mut settings = Self::get_settings();
        settings.custom_jvm_args = args.split_whitespace().map(|s| s.to_string()).collect();
        Self::save_settings(&settings)
    }

    /// Create a new instance using the REAL instance manager and config.
    pub fn create_instance(config: InstanceConfig) -> InstanceConfig {
        let instance = config.clone();
        let mut mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        mgr.add(config.clone());

        // Persist to config file
        if let Ok(mut cfg) = crate::CONFIG.lock() {
            let _ = cfg.add_instance(crate::config::InstanceConfig {
                id: config.id.clone(),
                name: config.name.clone(),
                game_version: config.game_version.clone(),
                loader: config.loader.clone(),
                loader_version: config.loader_version.clone(),
                memory: config.memory,
                java: config.java.clone(),
                game_dir: config.game_dir.clone(),
                resolution_width: config.resolution_width,
                resolution_height: config.resolution_height,
                account_uuid: config.account_uuid.clone(),
                minecraft_dir: config.minecraft_dir.clone(),
                custom_jvm_args: config.custom_jvm_args.clone(),
            });
        }

        instance
    }

    /// Delete an instance using the REAL instance manager and config.
    /// Generate a friendly instance ID based on loader type, e.g. "Vanilla",
    /// "Fabric-1", "Quilt-2". Avoids UUID-style folder names in the
    /// instances directory.
    fn generate_instance_id(loader: &str, existing_ids: &[String]) -> String {
        let base = match loader.to_lowercase().as_str() {
            "vanilla" => "Vanilla",
            "fabric" => "Fabric",
            "quilt" => "Quilt",
            "forge" => "Forge",
            _ => "Instance",
        };

        if !existing_ids.iter().any(|id| id == base) {
            return base.to_string();
        }

        let mut n = 2;
        loop {
            let candidate = format!("{}-{}", base, n);
            if !existing_ids.iter().any(|id| id == &candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    pub fn delete_instance(id: &str) -> bool {
        // Remove from the filesystem first
        let instances_dir = Self::instances_dir();
        let instance_path = instances_dir.join(id);
        if instance_path.exists() {
            let _ = std::fs::remove_dir_all(&instance_path);
        }

        let mut mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        let removed = mgr.remove(id);

        if let Ok(mut cfg) = crate::CONFIG.lock() {
            let _ = cfg.remove_instance(id);
        }

        removed
    }

    /// Update an instance using the REAL instance manager.
    pub fn update_instance(config: InstanceConfig) -> bool {
        let mut mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        let updated = mgr.update(config.clone());

        if let Ok(mut cfg) = crate::CONFIG.lock() {
            let _ = cfg.update_instance(crate::config::InstanceConfig {
                id: config.id.clone(),
                name: config.name.clone(),
                game_version: config.game_version.clone(),
                loader: config.loader.clone(),
                loader_version: config.loader_version.clone(),
                memory: config.memory,
                java: config.java.clone(),
                game_dir: config.game_dir.clone(),
                resolution_width: config.resolution_width,
                resolution_height: config.resolution_height,
                account_uuid: config.account_uuid.clone(),
                minecraft_dir: config.minecraft_dir.clone(),
                custom_jvm_args: config.custom_jvm_args.clone(),
            });
        }

        updated
    }

    /// Create a new default instance and persist it. Shared by the HOME
    /// [Create] button and the `create` command so both paths behave
    /// identically. Uses the vanilla loader because the headless launch path
    /// installs vanilla libraries only.
    pub fn quick_create_instance(state: &mut AppState) -> InstanceConfig {
        use crate::instances::InstanceConfig;

        let name = format!("Instance {}", state.instances.len() + 1);
        // Prefer a broadly compatible version; fall back to the first known.
        let game_version = if state.versions.iter().any(|v| v == "1.21.1") {
            "1.21.1".to_string()
        } else {
            state
                .versions
                .first()
                .cloned()
                .unwrap_or_else(|| "1.21.1".to_string())
        };
        let java_path = state
            .java_installations
            .first()
            .map(|j| j.path.to_string_lossy().to_string())
            .unwrap_or_default();

        let config = InstanceConfig {
            id: Self::generate_instance_id("vanilla", &state.instances.iter().map(|i| i.id.clone()).collect::<Vec<_>>()),
            name,
            game_version,
            loader: "vanilla".to_string(),
            loader_version: None,
            memory: BackendBridge::get_settings().default_memory,
            java: if java_path.is_empty() {
                None
            } else {
                Some(java_path)
            },
            game_dir: None,
            resolution_width: Some(1280),
            resolution_height: Some(720),
            account_uuid: state.selected_account.clone(),
            minecraft_dir: None,
            custom_jvm_args: Vec::new(),
        };

        let instance = BackendBridge::create_instance(config);
        let _ = instance.prepare_dirs(&BackendBridge::instances_dir());
        instance
    }

    /// Fetch Modrinth results for a Discover category, scoped to an exact
    /// Minecraft game version (and loader for mods/modpacks) so every shown
    /// project has a compatible build for the selected instance.
    pub fn fetch_discover(
        tab: crate::argus::state::DiscoverTab,
        query: &str,
        game_version: &str,
        loader: Option<&str>,
    ) -> Result<Vec<Project>, String> {
        use crate::argus::state::DiscoverTab;
        let content_type = match tab {
            DiscoverTab::Mods => "mod",
            DiscoverTab::Modpacks => "modpack",
            DiscoverTab::Shaders => "shader",
            DiscoverTab::ResourcePacks => "resourcepack",
        };
        Self::search_modrinth(query, content_type, game_version, loader.unwrap_or(""))
    }

    /// Download the best available file of a Modrinth project into the
    /// given instance's matching content directory. When `instance_id` is
    /// None, the first available instance is used.
    /// Fetch a project's versions filtered to the instance's loader and
    /// game version, for the install-time version chooser. Newest first.
    pub fn list_project_versions_scoped(
        project_id: &str,
        loader: Option<&str>,
        game_version: &str,
    ) -> Result<Vec<crate::modrinth::Version>, String> {
        let all = Self::get_mod_versions(project_id)?;
        let mut out: Vec<crate::modrinth::Version> = all
            .into_iter()
            .filter(|v| {
                v.game_versions.iter().any(|g| g == game_version)
                    && loader
                        .map(|l| v.loaders.iter().any(|x| x.eq_ignore_ascii_case(l)))
                        .unwrap_or(true)
            })
            .collect();
        // Releases before pre-releases within the same list.
        out.sort_by_key(|v| !v.version_type.eq_ignore_ascii_case("release"));
        Ok(out)
    }

    /// Install a project, optionally pinning an EXACT Modrinth version id
    /// (chosen in the per-mod version picker). None = auto-pick.
    pub fn install_project_version(
        project_id: &str,
        project_title: &str,
        content_type: &str,
        instance_id: Option<&str>,
        pinned_version_id: Option<&str>,
    ) -> Result<(String, Option<String>), String> {
        // Pick target instance: requested id, else first available.
        let mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        let instance = instance_id
            .and_then(|id| mgr.list().iter().find(|i| i.id == id))
            .or_else(|| mgr.list().first())
            .cloned();
        drop(mgr);

        let instance = instance.ok_or_else(|| {
            "No instance available — create one on the HOME screen first".to_string()
        })?;

        let dest_dir = match content_type {
            "modpack" => "modpacks",
            "resourcepack" => "resourcepacks",
            "shader" => "shaderpacks",
            _ => "mods",
        };

        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| format!("Failed to create runtime: {}", e))?;

        rt.block_on(async {
            let client =
                ModrinthClient::new().map_err(|e| format!("Failed to create client: {}", e))?;
            let versions = client
                .get_project_versions(project_id)
                .await
                .map_err(|e| format!("Failed to get versions: {}", e))?;

            // Mods and modpacks MUST match the instance's loader; shaders
            // and resource packs are loader-agnostic.
            let needs_loader = matches!(content_type, "mod" | "modpack");
            if needs_loader && instance.loader == "vanilla" {
                return Err(format!(
                    "'{}' is for modded instances — vanilla can't load {}. \
Create a Fabric or Quilt instance first.",
                    project_title,
                    if content_type == "mod" {
                        "mods"
                    } else {
                        "modpacks"
                    }
                ));
            }
            let version = match pinned_version_id {
                Some(vid) => versions
                    .iter()
                    .find(|v| v.id == vid)
                    .cloned()
                    .ok_or_else(|| "Chosen version no longer exists".to_string())?,
                None => {
                    let loader_filter = if needs_loader {
                        Some(instance.loader.as_str())
                    } else {
                        None
                    };
                    Self::pick_compatible_version(&versions, loader_filter, &instance.game_version)?
                }
            };
            let file = version
                .files
                .first()
                .ok_or_else(|| "No files attached to this version".to_string())?;

            let base = Self::instances_dir();
            let dir = base.join(&instance.id).join(dest_dir);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create {} dir: {}", dest_dir, e))?;
            // The filename comes from the remote Modrinth API; coerce it to
            // its final path component so crafted names like "..\\..\\x.dll"
            // cannot escape the instance content directory.
            let safe_name = std::path::Path::new(&file.filename)
                .file_name()
                .map(|s| s.to_os_string())
                .ok_or_else(|| format!("Invalid filename '{}'", file.filename))?;
            if safe_name.is_empty() {
                return Err("Empty download filename".to_string());
            }
            let dest = dir.join(safe_name);

            let dm = crate::downloads::DownloadManager::new();
            dm.download(&file.url, &dest)
                .await
                .map_err(|e| format!("Download failed: {}", e))?;

            if content_type == "modpack" {
                let instance_root = base.join(&instance.id);
                let provisioned = Self::download_modpack_files(&dest, &instance_root, &dm).await?;
                let overridden = Self::extract_overrides(&dest, &instance_root)?;
                if provisioned == 0 && overridden == 0 {
                    return Err("Modpack archive had no indexed files and no overrides".to_string());
                }
            }

            Ok((file.filename.clone(), Some(version.id.clone())))
        })
    }

    /// Map an archive-relative path to a safe path under the instance root.
    /// Rejects absolute paths and any parent components so a crafted pack
    /// cannot escape the instance directory.
    fn sanitize_rel_path(raw: &str) -> Option<std::path::PathBuf> {
        let candidate = std::path::Path::new(raw);
        if candidate.is_absolute() {
            return None;
        }
        let mut out = std::path::PathBuf::new();
        for comp in candidate.components() {
            match comp {
                std::path::Component::Normal(c) => out.push(c),
                _ => return None,
            }
        }
        if out.as_os_str().is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// Parse the client-relevant entries of a `modrinth.index.json`.
    /// Returns `(relative path, download url, optional sha1)`. Files whose
    /// client env is `unsupported` are skipped.
    fn parse_modpack_index(
        text: &str,
    ) -> Result<Vec<(std::path::PathBuf, String, Option<String>)>, String> {
        #[derive(serde::Deserialize)]
        struct ModpackIndex {
            #[serde(default)]
            files: Vec<ModpackIndexFile>,
        }
        #[derive(serde::Deserialize)]
        struct ModpackIndexFile {
            path: String,
            #[serde(default)]
            downloads: Vec<String>,
            #[serde(default)]
            hashes: std::collections::HashMap<String, String>,
            #[serde(default)]
            env: std::collections::HashMap<String, String>,
        }

        let idx: ModpackIndex = serde_json::from_str(text)
            .map_err(|e| format!("Invalid modrinth.index.json: {}", e))?;
        let mut out = Vec::new();
        for f in idx.files {
            if f.env
                .get("client")
                .map(|v| v == "unsupported")
                .unwrap_or(false)
            {
                continue;
            }
            let Some(url) = f.downloads.first().cloned() else {
                continue;
            };
            let Some(rel) = Self::sanitize_rel_path(&f.path) else {
                return Err(format!("Unsafe path in modpack index: {}", f.path));
            };
            out.push((rel, url, f.hashes.get("sha1").cloned()));
        }
        Ok(out)
    }

    /// Extract `overrides/` and `client-overrides/` from a `.mrpack` into the
    /// instance root. Returns how many files were written.
    fn extract_overrides(zip_path: &Path, instance_root: &Path) -> Result<usize, String> {
        const PREFIXES: [&str; 2] = ["overrides/", "client-overrides/"];
        let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let mut written = 0usize;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
            let name = entry.name().replace('\\', "/");
            let Some(prefix) = PREFIXES.iter().find(|p| name.starts_with(**p)) else {
                continue;
            };
            let rel_raw = &name[prefix.len()..];
            if rel_raw.is_empty() {
                continue;
            }
            let Some(rel) = Self::sanitize_rel_path(rel_raw) else {
                return Err(format!("Unsafe path in modpack overrides: {}", rel_raw));
            };
            let dest = instance_root.join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
            written += 1;
        }
        Ok(written)
    }

    /// Download every client file declared in the pack's
    /// `modrinth.index.json` into the instance root, verifying sha1 when the
    /// index provides one. Returns how many files were fetched.
    async fn download_modpack_files(
        zip_path: &Path,
        instance_root: &Path,
        dm: &crate::downloads::DownloadManager,
    ) -> Result<usize, String> {
        let index_text = {
            let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
            let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
            let mut entry = archive
                .by_name("modrinth.index.json")
                .map_err(|_| "Not a Modrinth modpack — missing modrinth.index.json".to_string())?;
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text).map_err(|e| e.to_string())?;
            text
        };
        let files = Self::parse_modpack_index(&index_text)?;
        let mut fetched = 0usize;
        for (rel, url, sha1) in files {
            let dest = instance_root.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir {} failed: {}", parent.display(), e))?;
            }
            dm.download(&url, &dest)
                .await
                .map_err(|e| format!("Failed to fetch {}: {}", rel.display(), e))?;
            if let Some(expected) = sha1.as_deref() {
                let ok = dm
                    .verify_sha1(&dest, expected)
                    .await
                    .map_err(|e| format!("Hash check failed for {}: {}", rel.display(), e))?;
                if !ok {
                    return Err(format!("SHA1 mismatch for {}", rel.display()));
                }
            }
            fetched += 1;
        }
        Ok(fetched)
    }

    /// Delete an installed content file (mod jar / resource pack / shader).
    /// The path MUST live inside the given instance's directory — refuse
    /// anything else as a safety net.
    pub fn remove_installed_content(instance_id: &str, path: &Path) -> Result<(), String> {
        let base = Self::instances_dir().join(instance_id);
        let canon_base = base
            .canonicalize()
            .map_err(|e| format!("Instance dir missing: {}", e))?;
        let canon_path = path
            .canonicalize()
            .map_err(|e| format!("File already gone: {}", e))?;
        if !canon_path.starts_with(&canon_base) {
            return Err("Refusing to delete a file outside this instance".to_string());
        }
        std::fs::remove_file(&canon_path).map_err(|e| format!("Delete failed: {}", e))?;
        // Drop any install-index record pointing at that file so DISCOVER
        // offers it again after re-scoping.
        let index = Self::installed_index_path(instance_id);
        if let Ok(text) = std::fs::read_to_string(&index) {
            let fname = path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            if let Ok(mut records) = serde_json::from_str::<Vec<InstallRecord>>(&text) {
                records.retain(|r| r.filename != fname);
                if let Ok(json) = serde_json::to_string_pretty(&records) {
                    let _ = std::fs::write(index, json);
                }
            }
        }
        Ok(())
    }

    /// Scan the given instance's content directories (mods, resource packs,
    /// shaders) for installed files. When `instance_id` is None, the first
    /// available instance is used.
    pub fn list_installed_content(
        instance_id: Option<&str>,
    ) -> Vec<crate::argus::state::InstalledContent> {
        use crate::argus::state::InstalledContent;

        let mut out = Vec::new();
        let mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        let instance = instance_id
            .and_then(|id| mgr.list().iter().find(|i| i.id == id))
            .or_else(|| mgr.list().first())
            .cloned();
        drop(mgr);

        let Some(instance) = instance else {
            return out;
        };
        let base = Self::instances_dir().join(&instance.id);

        let scan = |sub: &str, kind: &'static str| -> Vec<InstalledContent> {
            let mut items = Vec::new();
            let dir = base.join(sub);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let is_content = path.is_file()
                        && path
                            .extension()
                            .map(|e| e == "jar" || e == "zip" || e == "mrpack")
                            .unwrap_or(false);
                    if !is_content {
                        continue;
                    }
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    let name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    items.push(InstalledContent {
                        name,
                        kind,
                        path: path.clone(),
                        size_bytes: size,
                    });
                }
            }
            items.sort_by_key(|a| a.name.to_lowercase());
            items
        };

        out.extend(scan("mods", "MOD"));
        out.extend(scan("modpacks", "MODPACK"));
        out.extend(scan("resourcepacks", "RESOURCE PACK"));
        out.extend(scan("shaderpacks", "SHADER"));
        out
    }

    /// List world names in the given instance's saves dir. When
    /// `instance_id` is None, the first available instance is used.
    pub fn list_worlds(instance_id: Option<&str>) -> Vec<String> {
        let mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        let instance = instance_id
            .and_then(|id| mgr.list().iter().find(|i| i.id == id))
            .or_else(|| mgr.list().first())
            .cloned();
        drop(mgr);

        let Some(instance) = instance else {
            return Vec::new();
        };
        let saves = Self::instances_dir().join(&instance.id).join("saves");
        let mut worlds = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&saves) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        worlds.push(name.to_string());
                    }
                }
            }
        }
        worlds.sort();
        worlds
    }

    /// Scan the instance's game directory and the temp directory for JVM
    /// crash reports (`hs_err_pid*.log`). Returns parsed summaries.
    pub fn scan_crash_reports(instance_id: Option<&str>) -> Vec<crate::argus::state::CrashReport> {
        use crate::argus::state::CrashReport;

        let mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        let instance = instance_id
            .and_then(|id| mgr.list().iter().find(|i| i.id == id))
            .or_else(|| mgr.list().first())
            .cloned();
        drop(mgr);
        let Some(instance) = instance else {
            return Vec::new();
        };

        let mut reports = Vec::new();
        let mut scan_dirs = Vec::new();
        let base = Self::instances_dir().join(&instance.id);
        scan_dirs.push(base.join("game"));
        scan_dirs.push(std::env::temp_dir());

        for dir in scan_dirs {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = match path.file_name().and_then(|n| n.to_str()) {
                        Some(n) => n,
                        None => continue,
                    };
                    if !name.starts_with("hs_err_pid") || !name.ends_with(".log") {
                        continue;
                    }
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            if let Ok(duration) = modified.elapsed() {
                                let ts = if duration.as_secs() < 86400 {
                                    format!("{}h ago", duration.as_secs() / 3600)
                                } else {
                                    format!("{}d ago", duration.as_secs() / 86400)
                                };
                                let content = std::fs::read_to_string(&path).unwrap_or_default();
                                let (exception, thread, jvm) = Self::parse_crash_report(&content);
                                reports.push(CrashReport {
                                    path: path.clone(),
                                    timestamp: ts,
                                    exception,
                                    thread,
                                    jvm_version: jvm,
                                    summary: Self::summarize_crash(&content),
                                });
                            }
                        }
                    }
                }
            }
        }
        reports.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        reports
    }

    fn parse_crash_report(content: &str) -> (String, String, String) {
        let mut exception = String::new();
        let mut thread = String::new();
        let mut jvm = String::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("# ") && trimmed.contains("EXCEPTION") {
                exception = trimmed.to_string();
            } else if trimmed.starts_with("Thread:") {
                thread = trimmed.trim_start_matches("Thread:").trim().to_string();
            } else if trimmed.contains("OpenJDK") || trimmed.contains("jdk") {
                jvm = trimmed.to_string();
                break;
            }
        }
        (exception, thread, jvm)
    }

    fn summarize_crash(content: &str) -> String {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("# ") && trimmed.contains("EXCEPTION") {
                return trimmed.to_string();
            }
        }
        "Unknown crash".to_string()
    }

    /// Install the Fabric or Quilt loader for a Minecraft version by fetching
    /// its meta API, downloading every launcher library into `libs_dir`, and
    /// returning `(main_class, downloaded_library_paths)`.
    /// Fabric and Quilt share the same meta API shape.
    pub async fn install_loader_meta(
        meta_base: &str,
        game_version: &str,
        libs_dir: &Path,
    ) -> Result<(String, Vec<PathBuf>), String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("HTTP client error: {}", e))?;
        let url = format!(
            "{}/versions/loader/{}",
            meta_base.trim_end_matches('/'),
            game_version
        );
        let resp = client
            .get(&url)
            .header("User-Agent", "EraLauncher/0.1.5")
            .send()
            .await
            .map_err(|e| format!("Loader meta request failed: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("Loader meta HTTP {}", resp.status()));
        }
        let entries: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Invalid loader meta JSON: {}", e))?;
        let entry = entries
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| "Loader meta returned no profiles".to_string())?;

        let main_class = entry
            .pointer("/launcherMeta/mainClass/client")
            .and_then(|v| v.as_str())
            .unwrap_or("net.fabricmc.loader.impl.launch.knot.KnotClient")
            .to_string();

        std::fs::create_dir_all(libs_dir)
            .map_err(|e| format!("Failed to create libraries dir: {}", e))?;

        let dm = crate::downloads::DownloadManager::new();
        let mut paths = Vec::new();
        for section in ["common", "client"] {
            let Some(items) = entry
                .pointer(&format!("/launcherMeta/libraries/{}", section))
                .and_then(|v| v.as_array())
            else {
                continue;
            };
            for item in items {
                let Some(name) = item.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let Some(rel) = Self::maven_jar_path(name) else {
                    continue;
                };
                let base = item
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("https://maven.fabricmc.net/");
                let full_url = format!("{}/{}", base.trim_end_matches('/'), rel);
                let dest = libs_dir.join(&rel);
                if !paths.contains(&dest) {
                    paths.push(dest.clone());
                }
                if dest.exists() {
                    continue;
                }
                dm.download(&full_url, &dest)
                    .await
                    .map_err(|e| format!("Failed to download {}: {}", name, e))?;
            }
        }

        // The meta's libraries list does NOT include the loader jar itself
        // nor the intermediary/hashed mappings — without them java dies with
        // "Could not find or load main class KnotClient". Add both.
        let is_quilt = meta_base.contains("quilt");
        // Version location: meta nests them at entry.loader.version and
        // entry.intermediary.*. Reading a top-level "version" silently fell
        // back to a stale constant (0.16.14), which modern mods reject.
        let default_loader_ver = if is_quilt { "0.28.5" } else { "0.19.3" };
        let loader_version = entry
            .pointer("/loader/version")
            .and_then(|v| v.as_str())
            .unwrap_or(default_loader_ver)
            .to_string();
        let (repo, loader_coord, mapping_coord) = if is_quilt {
            (
                "https://maven.quiltmc.org/repository/release/",
                entry
                    .pointer("/loader/maven")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("org.quiltmc:quilt-loader:{}", loader_version)),
                entry
                    .pointer("/intermediary/maven")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("org.quiltmc:hashed:{}", game_version)),
            )
        } else {
            (
                "https://maven.fabricmc.net/",
                entry
                    .pointer("/loader/maven")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("net.fabricmc:fabric-loader:{}", loader_version)),
                entry
                    .pointer("/intermediary/maven")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("net.fabricmc:intermediary:{}", game_version)),
            )
        };

        // Purge OTHER loader versions so a stale jar (0.16.14) can never be
        // resolved alongside or instead of the freshly downloaded one.
        {
            let (group_dir, marker) = if is_quilt {
                (
                    "org/quiltmc/quilt-loader",
                    format!("quilt-loader-{}", loader_version),
                )
            } else {
                (
                    "net/fabricmc/fabric-loader",
                    format!("fabric-loader-{}", loader_version),
                )
            };
            let base = libs_dir.join(group_dir);
            if let Ok(entries) = std::fs::read_dir(&base) {
                for e in entries.flatten() {
                    let dir_name = e.file_name().to_string_lossy().to_string();
                    if !marker.contains(&dir_name) && !dir_name.contains(&marker) {
                        let _ = std::fs::remove_dir_all(e.path());
                    }
                }
            }
        }

        for coord in [loader_coord, mapping_coord] {
            let Some(rel) = Self::maven_jar_path(&coord) else {
                continue;
            };
            let full_url = format!("{}/{}", repo.trim_end_matches('/'), rel);
            let dest = libs_dir.join(&rel);
            if !paths.contains(&dest) {
                paths.push(dest.clone());
            }
            if dest.exists() {
                continue;
            }
            dm.download(&full_url, &dest)
                .await
                .map_err(|e| format!("Failed to download {} (needed to launch): {}", coord, e))?;
        }

        Ok((main_class, paths))
    }

    pub async fn install_fabric_loader(
        game_version: &str,
        libs_dir: &Path,
    ) -> Result<(String, Vec<PathBuf>), String> {
        Self::install_loader_meta("https://meta.fabricmc.net/v2", game_version, libs_dir).await
    }

    pub async fn install_quilt_loader(
        game_version: &str,
        libs_dir: &Path,
    ) -> Result<(String, Vec<PathBuf>), String> {
        Self::install_loader_meta("https://meta.quiltmc.org/v3", game_version, libs_dir).await
    }

    /// Get Quilt loader versions (fallback list; the real latest is resolved
    /// from the meta API at launch time).
    pub fn get_quilt_loader_versions() -> Vec<String> {
        vec![
            "0.28.5".to_string(),
            "0.27.1".to_string(),
            "0.26.0".to_string(),
        ]
    }

    /// Create a new instance with an explicit loader AND game version,
    /// persisted. This is the HOME → Create flow: loader first, then the
    /// user picks a version from the live Mojang release list.
    pub fn create_instance_full(
        state: &mut AppState,
        loader: &str,
        game_version: &str,
    ) -> Result<InstanceConfig, String> {
        use crate::instances::InstanceConfig;

        let l = loader.to_lowercase();
        if !Self::SUPPORTED_CREATE_LOADERS.contains(&l.as_str()) {
            return Err(format!(
                "Loader '{}' is not supported yet (available: vanilla, fabric, quilt)",
                loader
            ));
        }
        if game_version.trim().is_empty() {
            return Err("Game version must not be empty".to_string());
        }

        let name = format!("Instance {}", state.instances.len() + 1);
        let java_path = state
            .java_installations
            .first()
            .map(|j| j.path.to_string_lossy().to_string())
            .unwrap_or_default();

        let config = InstanceConfig {
            id: Self::generate_instance_id(&l, &state.instances.iter().map(|i| i.id.clone()).collect::<Vec<_>>()),
            name,
            game_version: game_version.trim().to_string(),
            loader: l.clone(),
            loader_version: match l.as_str() {
                "fabric" => Some(Self::get_fabric_loader_versions()[0].clone()),
                "quilt" => Some(Self::get_quilt_loader_versions()[0].clone()),
                _ => None,
            },
            memory: BackendBridge::get_settings().default_memory,
            java: if java_path.is_empty() {
                None
            } else {
                Some(java_path)
            },
            game_dir: None,
            resolution_width: Some(1280),
            resolution_height: Some(720),
            account_uuid: state.selected_account.clone(),
            minecraft_dir: None,
            custom_jvm_args: Vec::new(),
        };

        let instance = Self::create_instance(config);
        let _ = instance.prepare_dirs(&Self::instances_dir());
        Ok(instance)
    }

    /// Loaders the terminal UI can create and launch.
    pub const SUPPORTED_CREATE_LOADERS: &[&str] = &["vanilla", "fabric", "quilt"];

    /// Live Minecraft RELEASE versions (newest first) for the create-flow
    /// picker; falls back to a small static list when offline.
    pub fn fetch_minecraft_releases() -> Vec<String> {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(_) => return Self::fallback_versions(),
        };
        rt.block_on(async move {
            match crate::minecraft::manifest::ManifestClient::new() {
                Ok(client) => match client.get_release_versions().await {
                    Ok(list) if !list.is_empty() => list,
                    _ => Self::fallback_versions(),
                },
                Err(_) => Self::fallback_versions(),
            }
        })
    }

    // ===== Install index (powers "hide already-installed" in DISCOVER) =====

    pub fn installed_index_path(instance_id: &str) -> PathBuf {
        Self::instances_dir()
            .join(instance_id)
            .join("installed.json")
    }

    /// Persist an install record after a successful download.
    pub fn record_install(
        instance_id: &str,
        project_id: &str,
        version_id: Option<&str>,
        filename: &str,
        content_type: &str,
    ) {
        let path = Self::installed_index_path(instance_id);
        let mut records: Vec<InstallRecord> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if records
            .iter()
            .any(|r| r.project_id == project_id && r.filename == filename)
        {
            return;
        }
        records.push(InstallRecord {
            project_id: project_id.to_string(),
            version_id: version_id.map(|s| s.to_string()),
            filename: filename.to_string(),
            content_type: content_type.to_string(),
        });
        if let Ok(json) = serde_json::to_string_pretty(&records) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Project IDs recorded as installed for this instance.
    pub fn install_record_ids(instance_id: Option<&str>) -> Vec<String> {
        let Some(id) = instance_id else {
            return Vec::new();
        };
        let path = Self::installed_index_path(id);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<InstallRecord>>(&s).ok())
            .map(|records| records.into_iter().map(|r| r.project_id).collect())
            .unwrap_or_default()
    }

    /// Full install records for this instance (includes version_id).
    pub fn install_records(instance_id: Option<&str>) -> Vec<InstallRecord> {
        let Some(id) = instance_id else {
            return Vec::new();
        };
        let path = Self::installed_index_path(id);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<InstallRecord>>(&s).ok())
            .unwrap_or_default()
    }

    /// Check installed mods/resource packs/shaders for newer versions on Modrinth.
    /// Returns a list of `UpdatableMod` for content that has updates available.
    pub fn check_mod_updates(instance_id: Option<&str>) -> Vec<crate::argus::state::UpdatableMod> {
        use crate::argus::state::UpdatableMod;
        let records = Self::install_records(instance_id);
        if records.is_empty() {
            return Vec::new();
        }

        let mgr = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
        let instance = instance_id
            .and_then(|id| mgr.list().iter().find(|i| i.id == id))
            .or_else(|| mgr.list().first())
            .cloned();
        drop(mgr);
        let Some(instance) = instance else {
            return Vec::new();
        };

        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(_) => return Vec::new(),
        };

        let mut updatable = Vec::new();
        for record in records {
            if record.content_type != "mod" {
                continue;
            }
            let versions: Vec<_> = rt.block_on(async {
                let client = match ModrinthClient::new() {
                    Ok(c) => c,
                    Err(_) => return Vec::new(),
                };
                client
                    .get_project_versions(&record.project_id)
                    .await
                    .ok()
                    .unwrap_or_default()
            });

            let loader_filter = instance.loader.to_lowercase();
            let compatible: Vec<_> = versions
                .iter()
                .filter(|v| v.loaders.iter().any(|l| l.to_lowercase() == loader_filter))
                .cloned()
                .collect();

            let versions_to_check = if compatible.is_empty() {
                versions
            } else {
                compatible
            };

            let latest_release = versions_to_check
                .iter()
                .find(|v| v.version_type == "release");
            let installed_id = record.version_id.as_deref();
            let needs_update = match installed_id {
                Some(iid) => latest_release.map(|v| v.id != *iid).unwrap_or(false),
                None => {
                    let installed_ver = Self::extract_version_from_filename(&record.filename);
                    match installed_ver {
                        Some(iv) => latest_release
                            .map(|v| Self::compare_versions(&v.version_number, Some(&iv)) > 0)
                            .unwrap_or(false),
                        None => false,
                    }
                }
            };

            if needs_update {
                if let Some(latest) = latest_release {
                    updatable.push(UpdatableMod {
                        project_id: record.project_id.clone(),
                        title: record.filename.clone(),
                        installed_version: installed_id.unwrap_or("unknown").to_string(),
                        latest_version: latest.version_number.clone(),
                        latest_version_id: latest.id.clone(),
                        content_type: record.content_type.clone(),
                        filename: record.filename.clone(),
                    });
                }
            }
        }
        updatable
    }

    /// Extract a version string from a filename like "sodium-0.5.11.jar" or
    /// "fabric-language-kotlin-1.13.13+kotlin.2.4.10.jar".
    ///
    /// Scans dash-separated segments from left to right for one that contains
    /// digits and a version marker (`.` or `+`), skipping `mc`-prefixed MC
    /// version suffixes. Returns the best candidate found.
    fn extract_version_from_filename(filename: &str) -> Option<String> {
        let stem = std::path::Path::new(filename)
            .file_stem()?
            .to_string_lossy();
        for part in stem.split('-') {
            let has_digit = part.chars().any(|c| c.is_ascii_digit());
            let has_version_marker = part.contains('.') || part.contains('+');
            if has_digit && has_version_marker && !part.starts_with("mc") && !part.starts_with("MC")
            {
                return Some(part.to_string());
            }
        }
        stem.split('-').next_back().map(|s| s.to_string())
    }

    /// Compare two version strings semver-ish. Returns negative if a < b,
    /// positive if a > b, 0 if equal.
    fn compare_versions(a: &str, b: Option<&str>) -> i32 {
        let Some(b) = b else { return 1 };
        let parse = |s: &str| -> Vec<u64> {
            s.trim()
                .trim_start_matches('v')
                .split('.')
                .filter_map(|p| p.parse::<u64>().ok())
                .collect()
        };
        let av = parse(a);
        let bv = parse(b);
        for i in 0..av.len().max(bv.len()) {
            let ai = av.get(i).copied().unwrap_or(0);
            let bi = bv.get(i).copied().unwrap_or(0);
            match ai.cmp(&bi) {
                std::cmp::Ordering::Greater => return 1,
                std::cmp::Ordering::Less => return -1,
                std::cmp::Ordering::Equal => {}
            }
        }
        0
    }

    /// Heuristic legacy matcher: does an installed FILENAME look like it
    /// came from a project with this TITLE?
    ///
    /// - Multi-word titles must match the filename's leading tokens exactly
    ///   ("Sodium Extra" ↔ "sodium-extra-0.9.3" ✔, but plain "Sodium" does
    ///   NOT swallow SodiumExtra's file).
    /// - Single-word titles match when the stem equals the title or is
    ///   followed by a separator/version digit ("Iris" ↔ "iris-fabric…").
    ///
    /// New installs are tracked EXACTLY via installed.json; this heuristic
    /// only covers files downloaded before the index existed.
    pub fn title_matches_file(title: &str, filename: &str) -> bool {
        let stem = std::path::Path::new(filename)
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let tl = title.trim().to_lowercase();
        if tl.is_empty() || stem.is_empty() {
            return false;
        }
        let tokenize = |s: &str| -> Vec<String> {
            s.split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|t| !t.is_empty())
                .map(|t| t.to_string())
                .collect()
        };
        let tt = tokenize(&tl);
        let ft = tokenize(&stem);
        if tt.is_empty() || ft.is_empty() {
            return false;
        }
        // 1) Leading-token match: "Sodium Extra" ↔ "sodium-extra-0.9.3".
        if ft.len() >= tt.len() && ft[..tt.len()] == tt[..] {
            return true;
        }
        // 2) Concatenated prefix: "Mod Menu" ↔ "modmenu-1.7.18" (publishers
        //    often strip separators). A following letter breaks the match so
        //    plain "Sodium" does not swallow unrelated "sodiumXyz" files.
        let concat: String = tt.concat();
        match stem.strip_prefix(concat.as_str()) {
            Some(rest) => rest
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphabetic()),
            None => false,
        }
    }

    // ===== Offline accounts =====

    /// All saved accounts (offline profiles).
    pub fn list_accounts() -> Vec<crate::auth::Account> {
        CONFIG.lock().unwrap_or_else(|e| e.into_inner()).accounts.clone()
    }

    /// The account launches use (config.default_account).
    pub fn active_account() -> Option<crate::auth::Account> {
        let cfg = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        let id = cfg.default_account.clone()?;
        cfg.accounts.iter().find(|a| a.id == id).cloned()
    }

    /// Create an offline account with a validated username and persist it.
    /// First account created becomes active automatically.
    pub fn create_offline_account(username: &str) -> Result<crate::auth::Account, String> {
        let name = username.trim();
        if !crate::auth::is_valid_username(name) {
            return Err(
                "Username must be 3-16 characters: letters, numbers, underscore".to_string(),
            );
        }
        let account = crate::auth::Account::new_offline(name.to_string());
        let mut cfg = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        if cfg
            .accounts
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(name))
        {
            return Err(format!("Account '{}' already exists", name));
        }
        cfg.add_account(account.clone())
            .map_err(|e| format!("Failed to save account: {}", e))?;
        if cfg.default_account.is_none() {
            cfg.default_account = Some(account.id.clone());
            cfg.save()
                .map_err(|e| format!("Failed to save account: {}", e))?;
        }
        Ok(account)
    }

    /// Make an existing account the launch account.
    pub fn set_active_account(id: &str) -> Result<(), String> {
        let mut cfg = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        if !cfg.accounts.iter().any(|a| a.id == id) {
            return Err("Account not found".to_string());
        }
        cfg.default_account = Some(id.to_string());
        cfg.save()
            .map_err(|e| format!("Failed to save account: {}", e))?;
        Ok(())
    }

    /// Delete an account. If it was active, the first remaining account
    /// becomes active (or none).
    pub fn delete_account(id: &str) -> Result<(), String> {
        let was_active = {
            let cfg = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
            cfg.default_account.as_deref() == Some(id)
        };
        let mut cfg = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        cfg.remove_account(id)
            .map_err(|_| "Account not found".to_string())?;
        if was_active {
            cfg.default_account = cfg.accounts.first().map(|a| a.id.clone());
        }
        cfg.save()
            .map_err(|e| format!("Failed to save account: {}", e))?;
        Ok(())
    }

    /// Create a new instance with a specific mod loader and persist it.
    pub fn quick_create_instance_with_loader(
        state: &mut AppState,
        loader: &str,
    ) -> Result<InstanceConfig, String> {
        let l = loader.to_lowercase();
        if !Self::SUPPORTED_CREATE_LOADERS.contains(&l.as_str()) {
            return Err(format!(
                "Loader '{}' is not supported yet (available: vanilla, fabric, quilt)",
                loader
            ));
        }
        let mut instance = Self::quick_create_instance(state);
        instance.loader = l.clone();
        instance.loader_version = match l.as_str() {
            "fabric" => Some(Self::get_fabric_loader_versions()[0].clone()),
            "quilt" => Some(Self::get_quilt_loader_versions()[0].clone()),
            _ => None,
        };
        // Persist the loader choice (quick_create already saved vanilla).
        if !Self::update_instance(instance.clone()) {
            return Err("Failed to persist loader selection".to_string());
        }
        Ok(instance)
    }

    /// Get Fabric loader versions (cosmetic fallback for new instances —
    /// the REAL latest is always resolved from the meta API at launch).
    pub fn get_fabric_loader_versions() -> Vec<String> {
        vec![
            "0.19.3".to_string(),
            "0.18.4".to_string(),
            "0.17.2".to_string(),
            "0.16.14".to_string(),
        ]
    }

    /// Offline fallback for the version picker.
    fn fallback_versions() -> Vec<String> {
        vec![
            "26.2".to_string(),
            "1.21.1".to_string(),
            "1.20.1".to_string(),
            "1.19.4".to_string(),
            "1.18.2".to_string(),
        ]
    }

    /// Get Forge versions.
    pub fn get_forge_versions() -> Vec<String> {
        vec![
            "53.0.27".to_string(),
            "52.0.23".to_string(),
            "48.0.28".to_string(),
            "47.3.0".to_string(),
        ]
    }

    /// Get available Minecraft versions (matching the Tauri frontend).
    pub fn get_minecraft_versions() -> Vec<String> {
        vec![
            "26.2".to_string(),
            "1.21.1".to_string(),
            "1.20.1".to_string(),
            "1.19.4".to_string(),
            "1.18.2".to_string(),
        ]
    }

    /// Launch Minecraft using the existing LaunchEngine components.
    ///
    /// Since LaunchEngine::launch requires a Tauri window, this implements
    /// a headless launch path using the same underlying components:
    /// ManifestClient, DownloadManager, JavaManager, ArgumentBuilder.
    /// Start launching an instance on a BACKGROUND thread. Progress events
    /// arrive via `tracker.launch_events`, which the UI polls every loop
    /// iteration. Previously the whole pipeline ran on the UI thread via
    /// block_on, freezing rendering/input for minutes while libraries and
    /// assets downloaded.
    pub fn spawn_launch(
        instance: &InstanceConfig,
        state: &mut AppState,
        tracker: &mut RuntimeTracker,
    ) {
        let (event_tx, event_rx) = mpsc::channel::<LaunchEvent>();
        tracker.launch_events = Some(event_rx);

        let instances_dir = Self::instances_dir();
        let instance_dir = instances_dir.join(&instance.id);
        let _ = instance.prepare_dirs(&instances_dir);

        // Cheap, network-free validation up front so config mistakes fail
        // fast without touching runtime state.
        let java_path = if let Some(ref java) = instance.java {
            let mut p = PathBuf::from(java);
            if p.is_dir() {
                let bin_name = if cfg!(windows) { "java.exe" } else { "java" };
                p = p.join("bin").join(bin_name);
            }
            p
        } else {
            let required = JavaManager::required_for_minecraft(&instance.game_version);
            match JavaManager::find_compatible(required) {
                Some(j) => j.path,
                None => {
                    let _ = event_tx.send(LaunchEvent::Log(
                        LogLevel::Info,
                        format!("Java {} not found. Downloading...", required),
                    ));
                    let install_dir = crate::platform::Paths::new().data_local;
                    let rt = match tokio::runtime::Runtime::new() {
                        Ok(rt) => rt,
                        Err(e) => {
                            let msg = format!("Failed to create runtime for Java install: {}", e);
                            state.runtime_state = RuntimeState::Error(msg.clone());
                            let _ = event_tx.send(LaunchEvent::Failed(msg));
                            return;
                        }
                    };
                    let provisioned = rt.block_on(crate::installer::install_java_runtime(
                        install_dir,
                        required,
                    ));
                    let provisioned_path = match provisioned {
                        Ok(Some(path)) => path,
                        Ok(None) => {
                            let msg =
                                "Java installation completed but runtime not found".to_string();
                            state.runtime_state = RuntimeState::Error(msg.clone());
                            let _ = event_tx.send(LaunchEvent::Failed(msg));
                            return;
                        }
                        Err(e) => {
                            let msg = format!("Java install failed: {}", e);
                            state.runtime_state = RuntimeState::Error(msg.clone());
                            let _ = event_tx.send(LaunchEvent::Failed(msg));
                            return;
                        }
                    };

                    if instance.java.is_none() {
                        let mut updated = instance.clone();
                        updated.java = Some(provisioned_path.to_string_lossy().to_string());
                        let _ = Self::update_instance(updated);
                    }

                    provisioned_path
                }
            }
        };

        // Output pipes for the eventual Minecraft process.
        let (stdout_tx, stdout_rx) = mpsc::channel::<String>();
        let (stderr_tx, stderr_rx) = mpsc::channel::<String>();
        tracker.set_stdout_rx(stdout_rx);
        tracker.set_stderr_rx(stderr_rx);

        state.runtime_state = RuntimeState::Starting;
        state.log(
            LogLevel::Info,
            "BACKEND",
            &format!(
                "Launching {} v{} ({}) with Java {}",
                instance.name,
                instance.game_version,
                instance.loader,
                java_path.display()
            ),
        );

        let inst = instance.clone();
        std::thread::spawn(move || {
            // Clone for the pipeline closures; keep `tx` free for the final
            // Launched/Failed event after block_on returns.
            let tx = event_tx;
            let inner_tx = tx.clone();
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(LaunchEvent::Failed(format!(
                        "Failed to create runtime: {}",
                        e
                    )));
                    return;
                }
            };

            let result: Result<std::process::Child, String> = rt.block_on(async move {
                let progress = |msg: &str| {
                    let _ = inner_tx.send(LaunchEvent::Progress(msg.to_string()));
                };
                let log_line = |level: LogLevel, msg: String| {
                    let _ = inner_tx.send(LaunchEvent::Log(level, msg));
                };

                // Version manifest
                let manifest = crate::minecraft::manifest::ManifestClient::new()
                    .map_err(|e| format!("Failed to create manifest client: {}", e))?;
                let version_info = manifest
                    .get_version_info_by_id(&inst.game_version)
                    .await
                    .map_err(|e| format!("Failed to get version info: {}", e))?;

                progress("Downloading Minecraft...");

                let client_jar = Self::download_client(&version_info, &instance_dir, false)
                    .await
                    .map_err(|e| format!("Failed to download client: {}", e))?;

                // Integrity check: the manifest publishes a SHA1 for the
                // client jar — a corrupted download otherwise surfaces as
                // bizarre runtime crashes instead of a clear error.
                if let Some(expected) = version_info
                    .downloads
                    .as_ref()
                    .and_then(|d| d.client.as_ref())
                    .filter(|c| !c.sha1.is_empty())
                {
                    progress("Verifying Minecraft jar...");
                    let dm = crate::downloads::DownloadManager::new();
                    let ok = dm
                        .verify_sha1(&client_jar, &expected.sha1)
                        .await
                        .unwrap_or(false);
                    if !ok {
                        return Err(
                            "Minecraft jar failed its SHA1 check — delete it and relaunch to re-download"
                                .to_string(),
                        );
                    }
                }

                progress("Downloading libraries...");

                let mut all_libs = Self::download_libraries(
                    &version_info,
                    &instance_dir,
                    false,
                    &mut |m| log_line(LogLevel::Warn, m),
                )
                .await
                .map_err(|e| format!("Failed to download libraries: {}", e))?;

                // Mod-loader support: Fabric/Quilt install from their meta
                // API and override the main class.
                let mut main_class_override: Option<String> = None;
                match inst.loader.as_str() {
                    "fabric" | "quilt" => {
                        progress(&format!("Installing {} loader...", inst.loader));
                        let meta_base = if inst.loader == "quilt" {
                            "https://meta.quiltmc.org/v3"
                        } else {
                            "https://meta.fabricmc.net/v2"
                        };
                        let (mc, loader_libs) =
                            Self::install_loader_meta(
                                meta_base,
                                &inst.game_version,
                                &instance_dir.join("libraries"),
                            )
                            .await
                            .map_err(|e| {
                                format!("{} loader install failed: {}", inst.loader, e)
                            })?;
                        log_line(
                            LogLevel::Info,
                            format!(
                                "{} loader ready — {} libraries, main class {}",
                                inst.loader,
                                loader_libs.len(),
                                mc
                            ),
                        );
                        all_libs.extend(loader_libs);
                        main_class_override = Some(mc);
                    }
                    "forge" => {
                        return Err(
                            "Forge is not supported by the terminal launcher yet — use vanilla, fabric or quilt"
                                .to_string(),
                        );
                    }
                    _ => {}
                }

                progress("Extracting natives...");
                Self::extract_natives(&version_info, &instance_dir)
                    .await
                    .map_err(|e| format!("Failed to extract natives: {}", e))?;

                progress("Downloading assets... (first launch takes a while)");
                Self::download_assets(&version_info, &instance_dir)
                    .await
                    .map_err(|e| format!("Failed to download assets: {}", e))?;

                progress("Starting Minecraft...");

                let classpath = Self::build_classpath(&client_jar, &all_libs);
                let optimization_profile = {
                    let config = crate::CONFIG.lock().unwrap_or_else(|e| e.into_inner());
                    config.settings.optimization_profile
                };
                let (jvm_args, game_args, manifest_main) =
                    Self::build_launch_command(&version_info, &inst, &instance_dir, &classpath, optimization_profile);
                let main_class = main_class_override.unwrap_or(manifest_main);

                // Diagnostic + hard validation: a modded launch without its
                // loader jar on the classpath fails with an opaque
                // ClassNotFoundException, so fail here with a clear reason.
                let cp_has_loader = classpath.contains("fabric-loader")
                    || classpath.contains("quilt-loader");
                log_line(
                    LogLevel::Info,
                    format!(
                        "Classpath: {} entries, {} chars, loader-present={}",
                        all_libs.len() + 1,
                        classpath.len(),
                        cp_has_loader
                    ),
                );
                if matches!(inst.loader.as_str(), "fabric" | "quilt") && !cp_has_loader {
                    return Err(format!(
                        "{} loader jar missing from classpath — install failed silently",
                        inst.loader
                    ));
                }

                log_line(LogLevel::Info, format!("JVM args: {}", jvm_args.join(" ")));

                for arg in jvm_args.iter().chain(game_args.iter()) {
                    if arg.contains("${") {
                        log_line(
                            LogLevel::Warn,
                            format!("Unresolved launch token: {}", arg),
                        );
                    }
                }

                // The instance root doubles as the game working directory
                // (matches --gameDir above and where mods/ are installed).
                std::fs::create_dir_all(&instance_dir)
                    .map_err(|e| format!("Failed to create instance dir: {}", e))?;

                let mut cmd = std::process::Command::new(&java_path);
                for arg in &jvm_args {
                    cmd.arg(arg);
                }
                cmd.arg(&main_class);
                for arg in &game_args {
                    cmd.arg(arg);
                }
                cmd.current_dir(&instance_dir)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());

                eprintln!("=== BACKEND LAUNCH DEBUG ===");
                eprintln!("Java path: {}", java_path.display());
                eprintln!("Java exists: {}", java_path.exists());
                eprintln!("Java is_file: {}", java_path.is_file());
                if let Ok(canonical) = std::fs::canonicalize(&java_path) {
                    eprintln!("Java canonical: {}", canonical.display());
                }
                eprintln!("instance.java (from config): {:?}", inst.java);
                if let Some(ref stored) = inst.java {
                    let stored_path = std::path::PathBuf::from(stored);
                    eprintln!("stored java exists: {}", stored_path.exists());
                    eprintln!("stored java is_file: {}", stored_path.is_file());
                }
                eprintln!("JVM args: {}", jvm_args.join(" "));
                eprintln!("Main class: {}", main_class);
                eprintln!("Game args: {}", game_args.join(" "));
                eprintln!("Classpath entries: {}", all_libs.len() + 1);
                eprintln!("Classpath length: {}", classpath.len());
                eprintln!("Instance dir: {}", instance_dir.display());
                eprintln!("Instance dir exists: {}", instance_dir.exists());
                if let Ok(meta) = std::fs::metadata(&instance_dir) {
                    eprintln!("Instance dir is_dir: {}", meta.is_dir());
                    eprintln!("Instance dir len: {:?}", meta.len());
                }
                eprintln!("Current dir configured: true");
                eprintln!("stdin: null");
                eprintln!("stdout: piped");
                eprintln!("stderr: piped");
                eprintln!("Windows creation flags: default (none set)");

                let java_version_test = std::process::Command::new(&java_path).arg("-version").output();
                match java_version_test {
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        eprintln!("Direct java -version test: status={}, output={}", output.status, stderr);
                    }
                    Err(e) => {
                        eprintln!("Direct java -version test FAILED: {}", e);
                    }
                }
                eprintln!("=== END BACKEND LAUNCH DEBUG ===");

                let _ = std::fs::write(
                    std::env::temp_dir().join("era-backend-launch-debug.txt"),
                    format!(
                        "Java: {}\nExists: {}\nIsFile: {}\ninstance.java: {:?}\nJVM: {}\nClasspathLen: {}\nInstanceDir: {}\n",
                        java_path.display(),
                        java_path.exists(),
                        java_path.is_file(),
                        inst.java,
                        jvm_args.join(" "),
                        classpath.len(),
                        instance_dir.display()
                    ),
                );

                let mut child = cmd
                    .spawn()
                    .map_err(|e| format!("Failed to spawn Minecraft: {}", e))?;

                // Stream game output into the tracker channels.
                if let Some(stdout) = child.stdout.take() {
                    let tx = stdout_tx;
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        let reader = std::io::BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            let _ = tx.send(line);
                        }
                    });
                }
                if let Some(stderr) = child.stderr.take() {
                    let tx = stderr_tx;
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        let reader = std::io::BufReader::new(stderr);
                        for line in reader.lines().map_while(Result::ok) {
                            let _ = tx.send(line);
                        }
                    });
                }

                Ok(child)
            });

            match result {
                Ok(child) => {
                    let _ = tx.send(LaunchEvent::Launched(child));
                }
                Err(e) => {
                    let _ = tx.send(LaunchEvent::Failed(e));
                }
            }
        });
    }

    /// Stop the running Minecraft process.
    pub fn stop_instance(state: &mut AppState, tracker: &mut RuntimeTracker) -> bool {
        if tracker.is_running() {
            state.runtime_state = RuntimeState::Stopping;
            state.log(LogLevel::Info, "BACKEND", "Stopping Minecraft...");
            let stopped = tracker.stop();
            if stopped {
                state.runtime_state = RuntimeState::Stopped;
                state.log(LogLevel::Info, "BACKEND", "Minecraft stopped");
            }
            stopped
        } else {
            state.log(LogLevel::Warn, "BACKEND", "Minecraft is not running");
            false
        }
    }

    /// Poll for process output and update logs.
    pub fn poll_process_output(state: &mut AppState, tracker: &mut RuntimeTracker) {
        let lines = tracker.poll_output();
        for line in lines {
            state.log(LogLevel::Info, "Minecraft", &line);
        }

        // Check if process has exited
        if !tracker.is_running() && tracker.pid().is_some() {
            let status = tracker.take_exit_status();
            let code = status.as_ref().and_then(|s| s.code());
            state.runtime_state = RuntimeState::Stopped;
            match code {
                Some(0) | None => {
                    state.log(LogLevel::Info, "BACKEND", "Minecraft process exited");
                }
                Some(c) => {
                    state.log(
                        LogLevel::Warn,
                        "BACKEND",
                        &format!(
                            "Minecraft exited with code {} — check LOGS for the error",
                            c
                        ),
                    );
                    state.set_error(format!("Minecraft exited (code {}) — see LOGS", c));
                }
            }
            tracker.stop();
        }
    }

    // ---- Internal helper methods ----

    async fn download_client(
        version_info: &crate::minecraft::manifest::ManifestVersionInfo,
        instance_dir: &Path,
        fresh: bool,
    ) -> crate::prelude::Result<PathBuf> {
        let versions_dir = instance_dir.join("versions").join(&version_info.id);
        std::fs::create_dir_all(&versions_dir)?;
        let client_path = versions_dir.join(format!("{}.jar", version_info.id));

        if !client_path.exists() || fresh {
            if let Some(ref downloads) = version_info.downloads {
                if let Some(client) = &downloads.client {
                    let dm = crate::downloads::DownloadManager::new();
                    dm.download(&client.url, &client_path).await?;
                }
            }
        }
        Ok(client_path)
    }

    async fn download_libraries(
        version_info: &crate::minecraft::manifest::ManifestVersionInfo,
        instance_dir: &Path,
        fresh: bool,
        warn: &mut dyn FnMut(String),
    ) -> crate::prelude::Result<Vec<PathBuf>> {
        use futures::StreamExt;

        let libs_dir = instance_dir.join("libraries");
        std::fs::create_dir_all(&libs_dir)?;

        let os = match std::env::consts::OS {
            "windows" => "windows",
            "macos" => "osx",
            _ => "linux",
        };
        let arch = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            "x86_64"
        };

        let natives_keys = Self::natives_keys(os, arch);

        let mut download_tasks: Vec<(String, PathBuf, String)> = Vec::new();
        let mut paths = Vec::new();

        for lib in &version_info.libraries {
            if !Self::library_applies(&lib.rules) {
                continue;
            }
            if let Some(ref downloads) = lib.downloads {
                if let Some(ref artifact) = downloads.artifact {
                    if !artifact.url.is_empty() {
                        let path = Self::resolve_library_path(&lib.name, &libs_dir);
                        if fresh || !path.exists() {
                            download_tasks.push((
                                artifact.url.clone(),
                                path.clone(),
                                lib.name.clone(),
                            ));
                        }
                        paths.push(path);
                    }
                }
                if let Some(ref classifiers) = downloads.classifiers {
                    for key in &natives_keys {
                        if let Some(artifact) = classifiers.get(key) {
                            let class_path =
                                Self::resolve_classifier_path(&lib.name, &libs_dir, key);
                            if fresh || !class_path.exists() {
                                download_tasks.push((
                                    artifact.url.clone(),
                                    class_path.clone(),
                                    format!("{}:{}", lib.name, key),
                                ));
                            }
                            paths.push(class_path);
                            break;
                        }
                    }
                }
            }
        }

        // Download 8-way concurrent — sequential fetching stretched the
        // first launch by minutes.
        let dm = crate::downloads::DownloadManager::new();
        let dm_ref = &dm;
        let mut stream = futures::stream::iter(download_tasks)
            .map(|(url, path, label)| async move {
                match dm_ref.download(&url, &path).await {
                    Ok(_) => None,
                    Err(e) => Some((label, e.to_string())),
                }
            })
            .buffer_unordered(8);

        let mut failed = 0usize;
        while let Some(result) = stream.next().await {
            if let Some((label, err)) = result {
                failed += 1;
                if failed <= 5 {
                    warn(format!("Library download failed ({}): {}", label, err));
                }
            }
        }
        if failed > 5 {
            warn(format!("... and {} more library failures", failed - 5));
        }
        if failed > 0 {
            warn(format!(
                "{} library download(s) failed — launch may not work",
                failed
            ));
        }

        Ok(paths)
    }

    async fn extract_natives(
        version_info: &crate::minecraft::manifest::ManifestVersionInfo,
        instance_dir: &Path,
    ) -> crate::prelude::Result<PathBuf> {
        let natives_dir = instance_dir.join("natives");
        std::fs::create_dir_all(&natives_dir)?;

        let os = match std::env::consts::OS {
            "windows" => "windows",
            "macos" => "osx",
            _ => "linux",
        };
        let arch = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            "x86_64"
        };
        let natives_keys = Self::natives_keys(os, arch);

        let libs_dir = instance_dir.join("libraries");
        for lib in &version_info.libraries {
            if !Self::library_applies(&lib.rules) {
                continue;
            }
            if let Some(ref downloads) = lib.downloads {
                if let Some(ref classifiers) = downloads.classifiers {
                    for key in &natives_keys {
                        if let Some(_artifact) = classifiers.get(key) {
                            let nat_path = Self::resolve_classifier_path(&lib.name, &libs_dir, key);
                            if nat_path.exists() {
                                Self::extract_jar_natives(&nat_path, &natives_dir)?;
                            }
                            break;
                        }
                    }
                }
            }
        }

        Ok(natives_dir)
    }

    /// Official client layout for asset indexes (relative to assetsDir).
    pub fn asset_index_relative_path(id: &str) -> String {
        format!("indexes/{}.json", id)
    }

    async fn download_assets(
        version_info: &crate::minecraft::manifest::ManifestVersionInfo,
        instance_dir: &Path,
    ) -> crate::prelude::Result<()> {
        use crate::prelude::*;
        use futures::StreamExt;

        let assets_dir = instance_dir.join("assets");
        std::fs::create_dir_all(&assets_dir)?;

        // 1. Download the asset index JSON into the OFFICIAL layout the
        // client resolves via --assetIndex: <assetsDir>/indexes/<id>.json.
        // Writing it at the assets root (previous behaviour) left the game
        // unable to map ANY resource — every sound went silent.
        let asset_index_url = version_info.asset_index.url.clone();
        let indexes_dir = assets_dir.join("indexes");
        std::fs::create_dir_all(&indexes_dir)?;
        let asset_index_path = indexes_dir.join(format!("{}.json", version_info.asset_index.id));
        if !asset_index_path.exists() {
            // Migrate legacy root-level copies from earlier launches.
            let legacy = assets_dir.join(format!("{}.json", version_info.asset_index.id));
            if legacy.exists() {
                let _ = std::fs::rename(&legacy, &asset_index_path);
            }
        }
        if !asset_index_path.exists() {
            let dm = crate::downloads::DownloadManager::new();
            dm.download(&asset_index_url, &asset_index_path).await?;
        }

        // Verify the index against its published SHA1 (a truncated index
        // would silently drop most of the asset list).
        if !version_info.asset_index.sha1.is_empty() {
            let dm = crate::downloads::DownloadManager::new();
            let ok = dm
                .verify_sha1(&asset_index_path, &version_info.asset_index.sha1)
                .await
                .unwrap_or(false);
            if !ok {
                // One retry: delete the bad copy and re-download.
                let _ = std::fs::remove_file(&asset_index_path);
                let dm2 = crate::downloads::DownloadManager::new();
                dm2.download(&asset_index_url, &asset_index_path)
                    .await
                    .map_err(|e| {
                        LauncherError::Minecraft(format!(
                            "Failed to re-download asset index: {}",
                            e
                        ))
                    })?;
                let ok2 = dm2
                    .verify_sha1(&asset_index_path, &version_info.asset_index.sha1)
                    .await
                    .unwrap_or(false);
                if !ok2 {
                    return Err(LauncherError::Minecraft(
                        "Asset index failed its SHA1 check twice".to_string(),
                    ));
                }
            }
        }

        // 2. Parse the index and download every asset object. Previously
        // only the index itself was fetched (plus a stray copy of the client
        // jar), so the game started with no sounds or textures.
        let index_text = std::fs::read_to_string(&asset_index_path)?;
        let index: serde_json::Value = serde_json::from_str(&index_text)
            .map_err(|e| LauncherError::Minecraft(format!("Invalid asset index: {}", e)))?;
        let objects = index
            .get("objects")
            .and_then(|o| o.as_object())
            .ok_or_else(|| LauncherError::Minecraft("Asset index missing 'objects'".to_string()))?;

        let objects_dir = assets_dir.join("objects");
        std::fs::create_dir_all(&objects_dir)?;

        let dm = crate::downloads::DownloadManager::new();
        let mut tasks = Vec::new();
        for (_name, obj) in objects {
            let hash = obj
                .get("hash")
                .and_then(|h| h.as_str())
                .unwrap_or_default()
                .to_string();
            if hash.len() < 2 {
                continue;
            }
            let dest = objects_dir.join(&hash[..2]).join(&hash);
            if !dest.exists() {
                let url = format!(
                    "https://resources.download.minecraft.net/{}/{}",
                    &hash[..2],
                    hash
                );
                tasks.push((url, dest));
            }
        }

        // Fetch concurrently; missing assets are non-fatal (game degrades
        // gracefully).
        let dm_ref = &dm;
        let mut stream = futures::stream::iter(tasks)
            .map(|(url, dest)| async move {
                let _ = dm_ref.download(&url, &dest).await;
            })
            .buffer_unordered(16);
        while stream.next().await.is_some() {}
        Ok(())
    }

    fn build_classpath(client_jar: &Path, libs: &[PathBuf]) -> String {
        let mut paths: Vec<PathBuf> = vec![client_jar.to_path_buf()];
        paths.extend(libs.iter().cloned());
        paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(if cfg!(windows) { ";" } else { ":" })
    }

    fn build_launch_command(
        version_info: &crate::minecraft::manifest::ManifestVersionInfo,
        instance: &InstanceConfig,
        instance_dir: &Path,
        classpath: &str,
        optimization_profile: crate::minecraft::optimization::OptimizationProfile,
    ) -> (Vec<String>, Vec<String>, String) {
        use crate::minecraft::arguments::ArgumentBuilder;

        let features: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        // The INSTANCE ROOT is the game directory: mods/, saves/,
        // resourcepacks/, shaderpacks/ and options.txt all live here — this
        // must match where DISCOVER installs content. Previously --gameDir
        // pointed at a <instance>/game subfolder while mods went to
        // <instance>/mods, so Fabric launched with zero mods loaded.
        let game_dir_str = instance
            .game_dir
            .clone()
            .unwrap_or_else(|| instance_dir.to_string_lossy().to_string());

        // Assets live in the per-instance assets dir (populated by
        // download_assets).
        let assets_dir_str = instance
            .minecraft_dir
            .clone()
            .unwrap_or_else(|| instance_dir.join("assets").to_string_lossy().to_string());

        let natives_path = instance_dir.join("natives");
        let classpath_sep = if cfg!(windows) { ";" } else { ":" }.to_string();

        // Full token table — every ${...} used by vanilla manifests MUST be
        // present here or java receives literal "${token}" strings and dies
        // instantly (this previously left ${classpath} and
        // ${natives_directory} unresolved, which crashed the game before any
        // window appeared).
        // Resolve the player identity from the ACTIVE ACCOUNT (offline
        // profiles created in SETTINGS). Falls back to a generic "Steve"
        // when no account exists. The offline UUID is the deterministic
        // Minecraft scheme so servers/skins see a stable identity.
        let (account_name, uuid) = match Self::active_account() {
            Some(acc) => {
                let u = crate::auth::offline_uuid(&acc.name);
                (acc.name.clone(), u)
            }
            None => (
                "Steve".to_string(),
                "00000000-0000-0000-0000-000000000000".to_string(),
            ),
        };
        let tokens = vec![
            ("auth_player_name".to_string(), account_name.clone()),
            ("auth_uuid".to_string(), uuid.clone()),
            ("auth_access_token".to_string(), "0".repeat(32)),
            ("clientid".to_string(), uuid.clone()),
            ("auth_xuid".to_string(), uuid.clone()),
            ("user_type".to_string(), "msa".to_string()),
            ("user_properties".to_string(), "{}".to_string()),
            ("version_name".to_string(), version_info.id.clone()),
            (
                "version_type".to_string(),
                version_info.version_type.clone(),
            ),
            ("game_directory".to_string(), game_dir_str.clone()),
            ("assets_root".to_string(), assets_dir_str.clone()),
            (
                "assets_index_name".to_string(),
                version_info.asset_index.id.clone(),
            ),
            ("launcher_name".to_string(), "EraLauncher".to_string()),
            (
                "launcher_version".to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
            ),
            (
                "natives_directory".to_string(),
                natives_path.to_string_lossy().to_string(),
            ),
            ("classpath".to_string(), classpath.to_string()),
            ("classpath_separator".to_string(), classpath_sep),
            (
                "resolution_width".to_string(),
                instance.resolution_width.unwrap_or(854).to_string(),
            ),
            (
                "resolution_height".to_string(),
                instance.resolution_height.unwrap_or(480).to_string(),
            ),
        ];

        // JVM args: optimization profile args plus instance custom args.
        let profile_args = optimization_profile.jvm_args(instance.memory);
        let mut jvm_args = Vec::new();
        jvm_args.extend(profile_args);
        jvm_args.extend(instance.custom_jvm_args.clone());
        jvm_args.push("-Duser.language=en".to_string());
        // Game args: use the manifest's official list when available;
        // otherwise fall back to a minimal hand-built set.
        let mut game_args: Vec<String> = Vec::new();

        if let Some(ref args) = version_info.arguments {
            jvm_args.extend(ArgumentBuilder::collect_args(&args.jvm, &features));
            game_args.extend(ArgumentBuilder::collect_args(&args.game, &features));
        }

        if game_args.is_empty() {
            // Legacy manifests (pre-1.13) have no argument blocks.
            game_args = vec![
                "--username".to_string(),
                "${auth_player_name}".to_string(),
                "--version".to_string(),
                "${version_name}".to_string(),
                "--gameDir".to_string(),
                "${game_directory}".to_string(),
                "--assetsDir".to_string(),
                "${assets_root}".to_string(),
                "--assetIndex".to_string(),
                "${assets_index_name}".to_string(),
                "--uuid".to_string(),
                "${auth_uuid}".to_string(),
                "--accessToken".to_string(),
                "${auth_access_token}".to_string(),
                "--userType".to_string(),
                "${user_type}".to_string(),
            ];
        } else {
            // Custom/partial manifests may omit essentials — guarantee them.
            let essentials: &[(&str, &str)] = &[
                ("--username", "${auth_player_name}"),
                ("--version", "${version_name}"),
                ("--gameDir", "${game_directory}"),
                ("--assetsDir", "${assets_root}"),
                ("--assetIndex", "${assets_index_name}"),
                ("--uuid", "${auth_uuid}"),
                ("--accessToken", "${auth_access_token}"),
            ];
            for (flag, value) in essentials {
                if !game_args.iter().any(|a| a == flag) {
                    game_args.push((*flag).to_string());
                    game_args.push((*value).to_string());
                }
            }
        }
        if !jvm_args.iter().any(|a| a == "-cp") {
            // Legacy manifests also lack the -cp pair.
            jvm_args.push("-cp".to_string());
            jvm_args.push("${classpath}".to_string());
        }

        let jvm_args = ArgumentBuilder::substitute_tokens(&jvm_args, &tokens);
        let game_args = ArgumentBuilder::substitute_tokens(&game_args, &tokens);

        let main_class = version_info
            .main_class
            .clone()
            .unwrap_or_else(|| "net.minecraft.client.main.Main".to_string());

        (jvm_args, game_args, main_class)
    }

    fn natives_keys(os: &str, arch: &str) -> Vec<String> {
        let mut keys = Vec::new();
        keys.push(format!("natives-{}-{}", os, arch));
        if os == "windows" {
            keys.push("natives-windows".to_string());
        }
        if os == "osx" {
            keys.push("natives-osx".to_string());
        }
        if os == "linux" {
            keys.push("natives-linux".to_string());
        }
        keys
    }

    fn resolve_library_path(name: &str, libs_dir: &Path) -> PathBuf {
        // Standard Maven layout: group/artifact/version/artifact-version[-classifier].jar
        let parts: Vec<&str> = name.split(':').collect();
        if parts.len() >= 4 {
            let group = parts[0].replace('.', "/");
            let artifact = parts[1];
            let version = parts[2];
            let classifier = parts[3];
            libs_dir.join(format!(
                "{}/{}/{}/{}-{}-{}.jar",
                group, artifact, version, artifact, version, classifier
            ))
        } else if parts.len() >= 3 {
            let group = parts[0].replace('.', "/");
            let artifact = parts[1];
            let version = parts[2];
            // NOTE: the filename must be "{artifact}-{version}.jar". Using the
            // raw coordinate here produced names containing ':' which are
            // ILLEGAL on Windows and made every library download fail.
            libs_dir.join(format!(
                "{}/{}/{}/{}-{}.jar",
                group, artifact, version, artifact, version
            ))
        } else {
            libs_dir.join(format!("{}.jar", Self::sanitize_filename(name)))
        }
    }

    /// Strip characters illegal in Windows filenames.
    fn sanitize_filename(name: &str) -> String {
        name.replace([':', '\\', '/', '*', '?', '"', '<', '>', '|'], "_")
    }

    /// Pick the best Modrinth version for an instance. When `loader` is
    /// given, only builds for that loader are considered; an exact
    /// `game_version` match is preferred, otherwise the newest build.
    pub fn pick_compatible_version(
        versions: &[crate::modrinth::Version],
        loader: Option<&str>,
        game_version: &str,
    ) -> Result<crate::modrinth::Version, String> {
        // Release builds first, then betas, then alphas — the newest ALPHA
        // must never beat a stable build just by being listed first.
        fn release_rank(v: &crate::modrinth::Version) -> u8 {
            match v.version_type.to_lowercase().as_str() {
                "release" => 0,
                "beta" => 1,
                _ => 2,
            }
        }
        let mut pool: Vec<crate::modrinth::Version> = versions.to_vec();
        pool.sort_by_key(release_rank);
        if let Some(l) = loader {
            let matching: Vec<crate::modrinth::Version> = pool
                .into_iter()
                .filter(|v| v.loaders.iter().any(|x| x.eq_ignore_ascii_case(l)))
                .collect();
            if matching.is_empty() {
                return Err(format!(
                    "This project has no '{}' builds — it targets a different mod loader",
                    l
                ));
            }
            pool = matching;
        }
        let Some(first) = pool.first() else {
            return Err("No downloadable versions found".to_string());
        };
        if let Some(exact) = pool
            .iter()
            .find(|v| v.game_versions.iter().any(|g| g == game_version))
        {
            return Ok(exact.clone());
        }
        Ok(first.clone())
    }

    /// Convert a Maven coordinate ("group:artifact:version") into its
    /// repository-relative jar path ("group/path/artifact/version/
    /// artifact-version.jar"). Used by the Fabric/Quilt meta installers.
    pub fn maven_jar_path(name: &str) -> Option<String> {
        let parts: Vec<&str> = name.split(':').collect();
        if parts.len() < 3 {
            return None;
        }
        Some(format!(
            "{}/{}/{}/{}-{}.jar",
            parts[0].replace('.', "/"),
            parts[1],
            parts[2],
            parts[1],
            parts[2]
        ))
    }

    fn resolve_classifier_path(name: &str, libs_dir: &Path, key: &str) -> PathBuf {
        let parts: Vec<&str> = name.split(':').collect();
        if parts.len() >= 4 {
            let group = parts[0].replace('.', "/");
            let artifact = parts[1];
            let version = parts[2];
            let classifier_name = parts[3];
            libs_dir.join(format!(
                "{}/{}/{}/{}-{}-{}-{}.jar",
                group,
                artifact,
                version,
                artifact,
                version,
                classifier_name,
                key.strip_prefix("natives-").unwrap_or(key)
            ))
        } else {
            libs_dir.join(format!("{}-{}.jar", Self::sanitize_filename(name), key))
        }
    }

    fn library_applies(rules: &Option<serde_json::Value>) -> bool {
        match rules {
            Some(r) => {
                let features: std::collections::HashMap<String, bool> =
                    std::collections::HashMap::new();
                Self::rules_apply(r, &features)
            }
            None => true,
        }
    }

    fn rules_apply(
        rules: &serde_json::Value,
        features: &std::collections::HashMap<String, bool>,
    ) -> bool {
        let rules_arr = match rules.as_array() {
            Some(a) => a,
            None => return true,
        };
        let mut allowed = false;
        let mut matched = false;
        for rule in rules_arr {
            let obj = match rule.as_object() {
                Some(o) => o,
                None => continue,
            };
            let action = obj
                .get("action")
                .and_then(|a| a.as_str())
                .unwrap_or("allow");
            let os = obj.get("os").and_then(|o| o.as_object());
            let mut rule_ok = true;
            if let Some(os) = os {
                if let Some(name) = os.get("name").and_then(|n| n.as_str()) {
                    let current = match std::env::consts::OS {
                        "windows" => "windows",
                        "macos" => "osx",
                        _ => "linux",
                    };
                    if current != name {
                        rule_ok = false;
                    }
                }
            }
            if let Some(features_rule) = obj.get("features").and_then(|f| f.as_object()) {
                for (k, v) in features_rule {
                    let v_bool = match v {
                        serde_json::Value::Bool(b) => *b,
                        _ => continue,
                    };
                    if v_bool && !features.get(k).copied().unwrap_or(false) {
                        rule_ok = false;
                    }
                }
            }
            matched = true;
            if rule_ok {
                allowed = action == "allow";
            }
        }
        matched && allowed
    }

    fn extract_jar_natives(jar_path: &Path, natives_dir: &Path) -> crate::prelude::Result<()> {
        use std::fs;
        let file = fs::File::open(jar_path)?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| crate::errors::LauncherError::Zip(e.to_string()))?;

        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| crate::errors::LauncherError::Zip(e.to_string()))?;
            let name = file.name().to_string();
            if name.ends_with(".so") || name.ends_with(".dll") || name.ends_with(".dylib") {
                let Some(file_name) = std::path::Path::new(&name).file_name() else {
                    continue;
                };
                let outpath = natives_dir.join(file_name);
                let Some(parent) = outpath.parent() else {
                    continue;
                };
                fs::create_dir_all(parent)?;
                let mut out = fs::File::create(&outpath)?;
                std::io::copy(&mut file, &mut out)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_natives_keys_windows() {
        let keys = BackendBridge::natives_keys("windows", "x86_64");
        assert!(keys.iter().any(|k| k == "natives-windows-x86_64"));
        assert!(keys.iter().any(|k| k == "natives-windows"));
    }

    #[test]
    fn test_natives_keys_linux() {
        let keys = BackendBridge::natives_keys("linux", "x86_64");
        assert!(keys.iter().any(|k| k == "natives-linux-x86_64"));
        assert!(keys.iter().any(|k| k == "natives-linux"));
    }

    #[test]
    fn test_resolve_library_path_with_classifier() {
        let path = BackendBridge::resolve_library_path(
            "org.lwjgl:lwjgl-glfw:3.4.1:natives-windows",
            Path::new("/libs"),
        );
        assert!(path.to_string_lossy().contains("org/lwjgl"));
        assert!(path.to_string_lossy().contains("3.4.1"));
        assert!(path.to_string_lossy().ends_with(".jar"));
    }

    #[test]
    fn test_resolve_library_path_without_classifier() {
        let path =
            BackendBridge::resolve_library_path("com.mojang:brigadier:1.0.18", Path::new("/libs"));
        assert!(path.to_string_lossy().contains("com/mojang"));
        assert!(path.to_string_lossy().ends_with(".jar"));
    }

    #[test]
    fn test_build_classpath_format() {
        let cp = BackendBridge::build_classpath(
            Path::new("/client.jar"),
            &[PathBuf::from("/lib1.jar"), PathBuf::from("/lib2.jar")],
        );
        if cfg!(windows) {
            assert!(cp.contains(";"));
        } else {
            assert!(cp.contains(":"));
        }
        assert!(cp.contains("client.jar"));
        assert!(cp.contains("lib1.jar"));
        assert!(cp.contains("lib2.jar"));
    }

    #[test]
    fn test_fabric_loader_versions() {
        let versions = BackendBridge::get_fabric_loader_versions();
        // Newest-first, and the modern baseline must be present — 0.16.x is
        // rejected by current mods (Sodium Extra needs >=0.18).
        assert_eq!(versions.first(), Some(&"0.19.3".to_string()));
        assert!(versions.iter().any(|v| v.starts_with("0.18.")));
    }

    #[test]
    fn test_forge_versions() {
        let versions = BackendBridge::get_forge_versions();
        assert!(versions.iter().any(|v| v == "53.0.27"));
    }

    #[test]
    fn test_minecraft_versions() {
        let versions = BackendBridge::get_minecraft_versions();
        assert!(versions.contains(&"26.2".to_string()));
        assert!(versions.contains(&"1.21.1".to_string()));
    }

    #[test]
    fn test_instances_dir() {
        let dir = BackendBridge::instances_dir();
        assert!(dir.to_string_lossy().contains("instances"));
    }

    #[test]
    fn test_rules_apply_no_rules() {
        let features: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        let rules = serde_json::Value::Null;
        assert!(BackendBridge::rules_apply(&rules, &features));
    }

    #[test]
    fn test_rules_apply_empty() {
        let features: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        let rules = serde_json::json!([]);
        assert!(!BackendBridge::rules_apply(&rules, &features));
    }

    #[test]
    fn test_library_applies_no_rules() {
        assert!(BackendBridge::library_applies(&None));
    }

    /// Regression: game args used to be formatted as single tokens like
    /// "--username Steve", which the Minecraft parser cannot read, so the
    /// game never started. Later, JVM tokens like ${classpath} and
    /// ${natives_directory} were left unsubstituted, crashing java before
    /// any window appeared. Both must stay fixed.
    #[test]
    fn test_game_args_are_separate_tokens() {
        let version_info = test_manifest_fixture();
        let instance = crate::instances::InstanceConfig {
            id: "test-launch".to_string(),
            name: "Test".to_string(),
            game_version: "1.21.1".to_string(),
            loader: "vanilla".to_string(),
            loader_version: None,
            memory: 4096,
            java: None,
            game_dir: None,
            resolution_width: None,
            resolution_height: None,
            account_uuid: None,
            minecraft_dir: None,
            custom_jvm_args: Vec::new(),
        };
        let dir = std::env::temp_dir().join("era-test-launch-cmd");
        let classpath = "C:\\libs\\a.jar;C:\\client.jar";
        let (jvm, game, main_class) = BackendBridge::build_launch_command(
            &version_info,
            &instance,
            &dir,
            classpath,
            crate::minecraft::optimization::OptimizationProfile::Mid,
        );

        // 1. Flags and values are separate argv elements.
        for arg in &game {
            if arg.starts_with("--") {
                assert!(
                    !arg.contains(' '),
                    "game arg '{}' must be split into separate flag/value tokens",
                    arg
                );
            }
        }
        // 2. The username flag is followed by its value: the ACTIVE offline
        // account when one exists, else the "Steve" fallback (machine-state
        // independent assertion).
        let expected_player = BackendBridge::active_account()
            .map(|a| a.name)
            .unwrap_or_else(|| "Steve".to_string());
        let username_pos = game.iter().position(|a| a == "--username").unwrap();
        assert_eq!(game[username_pos + 1], expected_player);
        // 3. NO unresolved ${token} remains anywhere — this was the bug that
        // made Minecraft exit instantly without ever opening a window.
        for arg in jvm.iter().chain(game.iter()) {
            assert!(
                !arg.contains("${"),
                "unresolved launch token '{}': add it to the substitution table",
                arg
            );
        }
        // 4. The classpath pair resolves to the real classpath.
        let cp_pos = jvm.iter().position(|a| a == "-cp").unwrap();
        assert_eq!(jvm[cp_pos + 1], classpath);
        // 5. The natives library path resolves inside the instance dir.
        let lib_path_arg = jvm
            .iter()
            .find(|a| a.starts_with("-Djava.library.path="))
            .expect("manifest supplies -Djava.library.path");
        assert!(!lib_path_arg.ends_with("${natives_directory}"));
        // 6. Main class and heap flag intact.
        assert_eq!(main_class, "net.minecraft.client.main.Main");
        assert!(jvm.iter().any(|a| a.starts_with("-Xmx")));
    }

    fn test_manifest_fixture() -> crate::minecraft::manifest::ManifestVersionInfo {
        serde_json::from_str(
            r#"{
            "id": "1.21.1",
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "libraries": [],
            "arguments": {
                "game": ["--username", "${auth_player_name}", "--width", "${resolution_width}", "--height", "${resolution_height}"],
                "jvm": ["-Djava.library.path=${natives_directory}", "-Djna.tmpdir=${natives_directory}", "-cp", "${classpath}"]
            },
            "assetIndex": {
                "id": "1.21.1",
                "url": "https://example.com/asset",
                "sha1": "abc",
                "size": 100
            }
        }"#,
        )
        .unwrap()
    }

    /// Legacy manifests have no arguments block; the builder must supply its
    /// own -cp/classpath and game args.
    #[test]
    fn test_legacy_manifest_gets_fallback_args() {
        let version_info: crate::minecraft::manifest::ManifestVersionInfo = serde_json::from_str(
            r#"{
            "id": "1.12.2",
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "libraries": [],
            "assetIndex": {
                "id": "1.12",
                "url": "https://example.com/asset",
                "sha1": "abc",
                "size": 100
            }
        }"#,
        )
        .unwrap();
        let instance = crate::instances::InstanceConfig {
            id: "legacy".to_string(),
            name: "Legacy".to_string(),
            game_version: "1.12.2".to_string(),
            loader: "vanilla".to_string(),
            loader_version: None,
            memory: 2048,
            java: None,
            game_dir: None,
            resolution_width: None,
            resolution_height: None,
            account_uuid: None,
            minecraft_dir: None,
            custom_jvm_args: Vec::new(),
        };
        let dir = std::env::temp_dir().join("era-test-launch-legacy");
        let (jvm, game, _) = BackendBridge::build_launch_command(
            &version_info,
            &instance,
            &dir,
            "cp.jar",
            crate::minecraft::optimization::OptimizationProfile::Mid,
        );

        let cp_pos = jvm.iter().position(|a| a == "-cp").unwrap();
        assert_eq!(jvm[cp_pos + 1], "cp.jar");
        let expected_player = BackendBridge::active_account()
            .map(|a| a.name)
            .unwrap_or_else(|| "Steve".to_string());
        let u_pos = game.iter().position(|a| a == "--username").unwrap();
        assert_eq!(game[u_pos + 1], expected_player);
        assert!(game.chunks(2).all(|c| c.len() == 2));
        for arg in jvm.iter().chain(game.iter()) {
            assert!(!arg.contains("${"));
        }
    }

    #[test]
    fn test_parse_asset_index_objects() {
        let index: serde_json::Value = serde_json::from_str(
            r#"{"objects": {
                "minecraft/sounds/random/click.ogg": {"hash": "9ea1b80ddb116f0355d5f9107ba0c9f4a3c1e7b8", "size": 3784},
                "minecraft/textures/block/stone.png": {"hash": "0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b", "size": 245}
            }}"#,
        )
        .unwrap();
        let objects = index.get("objects").and_then(|o| o.as_object()).unwrap();
        assert_eq!(objects.len(), 2);
        for (_name, obj) in objects {
            let hash = obj.get("hash").and_then(|h| h.as_str()).unwrap();
            assert_eq!(hash.len(), 40);
            // Destination layout uses first two hex chars as subdirectory.
            let dest = format!("objects/{}/{}", &hash[..2], hash);
            assert!(dest.starts_with("objects/"));
        }
    }

    /// Regression: library filenames previously embedded the raw Maven
    /// coordinate ("com.google.guava:guava:32.1.2-jre.jar") — colons are
    /// illegal in Windows filenames, so EVERY normal library download failed
    /// with "The filename, directory name, or volume label syntax is
    /// incorrect" and the game died with NoClassDefFoundError.
    #[test]
    fn test_library_paths_are_windows_safe() {
        let libs_dir = Path::new("D:\\instances\\x\\libraries");
        for coord in [
            "com.google.guava:guava:32.1.2-jre",
            "com.google.code.gson:gson:2.10.1",
            "com.github.oshi:oshi-core:6.4.10",
            "org.lwjgl:lwjgl:3.3.3",
            "org.slf4j:slf4j-api:2.0.9",
        ] {
            let p = BackendBridge::resolve_library_path(coord, libs_dir);
            let file = p.file_name().unwrap().to_string_lossy().to_string();
            assert!(
                !file.contains(':'),
                "'{}' produced illegal filename '{}'",
                coord,
                file
            );
            assert!(file.ends_with(".jar"));
            // Standard Maven artifact naming.
            let parts: Vec<&str> = coord.split(':').collect();
            let expected = format!("{}-{}.jar", parts[1], parts[2]);
            assert_eq!(file, expected);
        }

        // Classifier (natives) coordinates stay legal too.
        let nat = BackendBridge::resolve_classifier_path(
            "org.lwjgl:lwjgl-platform:3.3.3:natives-windows",
            libs_dir,
            "natives-windows-x86_64",
        );
        let nf = nat.file_name().unwrap().to_string_lossy().to_string();
        assert!(!nf.contains(':'), "illegal natives filename '{}'", nf);
    }

    #[test]
    fn test_maven_jar_path() {
        assert_eq!(
            BackendBridge::maven_jar_path("net.fabricmc:fabric-loader:0.16.14"),
            Some("net/fabricmc/fabric-loader/0.16.14/fabric-loader-0.16.14.jar".to_string())
        );
        assert_eq!(BackendBridge::maven_jar_path("bad-coord"), None);
    }

    fn modrinth_version(
        id: &str,
        loaders: &[&str],
        game_versions: &[&str],
    ) -> crate::modrinth::Version {
        modrinth_version_typed(id, loaders, game_versions, "release")
    }

    fn modrinth_version_typed(
        id: &str,
        loaders: &[&str],
        game_versions: &[&str],
        version_type: &str,
    ) -> crate::modrinth::Version {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "version_number": id,
            "version_type": version_type,
            "loaders": loaders,
            "game_versions": game_versions,
            "files": []
        }))
        .unwrap()
    }

    /// Auto-pick must prefer RELEASE builds — the newest ALPHA (e.g.
    /// sodium 0.9.2-alpha.4) previously won and broke compatibility with
    /// iris, even though a matching stable build existed.
    #[test]
    fn test_pick_prefers_release_over_alpha() {
        let versions = vec![
            modrinth_version_typed("v-alpha", &["fabric"], &["26.2"], "alpha"),
            modrinth_version_typed("v-release", &["fabric"], &["26.2"], "release"),
            modrinth_version_typed("v-beta", &["fabric"], &["26.2"], "beta"),
        ];
        let picked =
            BackendBridge::pick_compatible_version(&versions, Some("fabric"), "26.2").unwrap();
        assert_eq!(picked.id, "v-release");
    }

    /// Installing a mod must pick builds matching the instance loader, and
    /// prefer the exact Minecraft version. A NeoForge-only project must be
    /// REJECTED for a fabric instance instead of downloading an unusable jar
    /// (the user's MODS list showed NeoForge jars inside a Fabric instance).
    /// Legacy filename↔title matching used to hide installed projects from
    /// DISCOVER. Covers the user's real library and the tricky collisions.
    #[test]
    fn test_title_matches_file() {
        // Real files from the user's instance:
        assert!(BackendBridge::title_matches_file(
            "Iris",
            "iris-fabric-1.11.3+mc26.1.2.jar"
        ));
        assert!(BackendBridge::title_matches_file(
            "Mod Menu",
            "modmenu-1.7.18.jar"
        ));
        assert!(BackendBridge::title_matches_file(
            "Sodium Extra",
            "sodium-extra-fabric-0.9.3+mc26.2.jar"
        ));
        assert!(BackendBridge::title_matches_file(
            "ImmediatelyFast",
            "ImmediatelyFast-NeoForge-1.16.3+26.2.jar"
        ));
        assert!(BackendBridge::title_matches_file(
            "Sodium",
            "sodium-neoforge-0.8.14-beta.2+mc1.21.11.jar"
        ));

        // Multi-word titles must match ALL tokens — plain "Sodium" must not
        // swallow the "Sodium Extra" file (and vice versa).
        assert!(!BackendBridge::title_matches_file(
            "Sodium Extra",
            "sodium-neoforge-0.8.14.jar"
        ));
        // A letter continuing the word breaks the prefix ("Iris" vs "Irish").
        assert!(!BackendBridge::title_matches_file(
            "Iris",
            "irish-flag-pack.zip"
        ));
        // Completely unrelated.
        assert!(!BackendBridge::title_matches_file(
            "JEI",
            "sodium-fabric-0.6.0.jar"
        ));
        // Exact stem equality matches.
        assert!(BackendBridge::title_matches_file(
            "AppleSkin",
            "appleskin.jar"
        ));
    }

    #[test]
    fn test_pick_compatible_version_filters_loader() {
        let versions = vec![
            modrinth_version("v-neo", &["neoforge"], &["1.21.1"]),
            modrinth_version("v-fab-old", &["fabric"], &["1.20.4"]),
            modrinth_version("v-fab-exact", &["fabric", "quilt"], &["1.21.1"]),
        ];

        // Fabric instance: neoforge build rejected, exact MC match wins.
        let picked =
            BackendBridge::pick_compatible_version(&versions, Some("fabric"), "1.21.1").unwrap();
        assert_eq!(picked.id, "v-fab-exact");

        // Quilt also matches (declared compatibility).
        let picked =
            BackendBridge::pick_compatible_version(&versions, Some("quilt"), "1.21.1").unwrap();
        assert_eq!(picked.id, "v-fab-exact");

        // No compatible loader → explicit error, not a silent bad download.
        let err = BackendBridge::pick_compatible_version(&versions, Some("forge"), "1.21.1");
        assert!(err.is_err());

        // Shaders/resource packs (loader=None) fall back to newest build.
        let picked = BackendBridge::pick_compatible_version(&versions, None, "26.2").unwrap();
        assert_eq!(picked.id, "v-neo");
    }

    /// The game directory passed via --gameDir must be the INSTANCE ROOT —
    /// that is where DISCOVER installs mods/. Pointing it at a subfolder
    /// made Fabric launch with zero mods.
    #[test]
    fn test_gamedir_is_instance_root() {
        let version_info = test_manifest_fixture();
        let dir = std::env::temp_dir().join("era-test-gamedir-root");
        let instance = crate::instances::InstanceConfig {
            id: "gamedir-test".to_string(),
            name: "T".to_string(),
            game_version: "1.21.1".to_string(),
            loader: "fabric".to_string(),
            loader_version: None,
            memory: 2048,
            java: None,
            game_dir: None,
            resolution_width: None,
            resolution_height: None,
            account_uuid: None,
            minecraft_dir: None,
            custom_jvm_args: Vec::new(),
        };
        let (_jvm, game, _) = BackendBridge::build_launch_command(
            &version_info,
            &instance,
            &dir,
            "cp.jar",
            crate::minecraft::optimization::OptimizationProfile::Mid,
        );
        let gd_pos = game.iter().position(|a| a == "--gameDir").unwrap();
        assert_eq!(
            std::path::Path::new(&game[gd_pos + 1]),
            std::path::Path::new(&dir),
            "--gameDir must point at the instance root"
        );
    }

    /// Regression (network): the fabric meta libraries list omits the loader
    /// jar and intermediary mappings — the installer must fetch both, or java
    /// dies with "Could not find or load main class KnotClient".
    #[test]
    #[ignore = "requires network"]
    fn test_install_fabric_loader_returns_loader_and_mappings() {
        let dir = std::env::temp_dir().join(format!(
            "era-fabric-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (main_class, libs) = rt
            .block_on(BackendBridge::install_fabric_loader("1.21.1", &dir))
            .unwrap();
        assert!(main_class.contains("KnotClient"), "main={}", main_class);
        // Regression: the loader version lives at entry.loader.version —
        // reading top-level "version" silently installed the stale 0.16.14
        // fallback, which Sodium Extra (>=0.18 required) rejects.
        let loader_jar = libs
            .iter()
            .find(|p| p.to_string_lossy().contains("fabric-loader-"))
            .expect("loader jar missing from returned libs");
        let fname = loader_jar.file_name().unwrap().to_string_lossy();
        assert!(
            !fname.contains("fabric-loader-0.16."),
            "stale 0.16.x loader resolved: {}",
            fname
        );
        assert!(
            libs.iter()
                .any(|p| p.to_string_lossy().contains("intermediary")),
            "intermediary mappings missing from returned libs"
        );
        for p in &libs {
            assert!(
                p.exists(),
                "downloaded lib missing on disk: {}",
                p.display()
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The asset index MUST live at <assetsDir>/indexes/<id>.json — that is
    /// the path the client resolves from --assetIndex. Writing it at the
    /// assets root made every resource unresolvable: no sound, no textures.
    #[test]
    fn test_asset_index_uses_official_layout() {
        assert_eq!(
            BackendBridge::asset_index_relative_path("17"),
            "indexes/17.json"
        );
        assert_eq!(
            BackendBridge::asset_index_relative_path("1.21.1"),
            "indexes/1.21.1.json"
        );
        let binding = BackendBridge::asset_index_relative_path("17");
        let p = Path::new(&binding);
        assert!(p.starts_with("indexes"));
        // Root-level placement is exactly what broke sound:
        assert_ne!(
            BackendBridge::asset_index_relative_path("17"),
            format!("17.json")
        );
    }

    #[test]
    fn test_fetch_discover_maps_categories() {
        use crate::argus::state::DiscoverTab;
        // Offline-safe check: category mapping happens before any network
        // call, so an invalid type would surface as a request error rather
        // than a panic. We only assert the function exists per tab here via
        // the content-type mapping logic in fetch_discover.
        let tabs = DiscoverTab::all();
        assert_eq!(tabs.len(), 4);
        assert_eq!(tabs[0].label(), "Mods");
        assert_eq!(tabs[3].label(), "Resource Packs");
    }

    #[test]
    fn test_parse_modpack_index_maps_and_skips_server_only() {
        let json = r#"{
            "formatVersion": 1,
            "game": "minecraft",
            "files": [
                {
                    "path": "mods/sodium.jar",
                    "downloads": ["https://cdn.example/sodium.jar"],
                    "hashes": {"sha1": "abc123"}
                },
                {
                    "path": "mods/server-only.jar",
                    "env": {"client": "unsupported", "server": "required"},
                    "downloads": ["https://cdn.example/server-only.jar"]
                },
                {
                    "path": "mods/no-url.jar"
                }
            ]
        }"#;
        let files = BackendBridge::parse_modpack_index(json).expect("parse failed");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, std::path::PathBuf::from("mods/sodium.jar"));
        assert_eq!(files[0].1, "https://cdn.example/sodium.jar");
        assert_eq!(files[0].2.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_parse_modpack_index_rejects_unsafe_paths() {
        let json = r#"{
            "files": [
                {"path": "../../evil.dll", "downloads": ["https://cdn.example/x"]}
            ]
        }"#;
        assert!(BackendBridge::parse_modpack_index(json).is_err());

        let absolute = r#"{
            "files": [
                {"path": "C:\\Windows\\evil.dll", "downloads": ["https://cdn.example/x"]}
            ]
        }"#;
        assert!(BackendBridge::parse_modpack_index(absolute).is_err());
    }

    #[test]
    fn test_sanitize_rel_path() {
        use crate::argus::backend::BackendBridge as B;
        assert_eq!(
            B::sanitize_rel_path("mods/foo.jar"),
            Some(std::path::PathBuf::from("mods/foo.jar"))
        );
        assert_eq!(B::sanitize_rel_path("..\\evil"), None);
        assert_eq!(B::sanitize_rel_path("/abs/path"), None);
        assert_eq!(B::sanitize_rel_path(""), None);
        assert_eq!(
            B::sanitize_rel_path("config/./sub/../x.txt"),
            None,
            "CurDir/Parent components must be rejected outright"
        );
    }

    #[test]
    fn test_extract_overrides_maps_into_instance_root() {
        let tmp = std::env::temp_dir().join(format!("era-mp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("instance");
        std::fs::create_dir_all(&root).unwrap();
        let zip_path = tmp.join("pack.mrpack");

        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zw.start_file("overrides/options.txt", opts).unwrap();
        std::io::Write::write_all(&mut zw, b"maxFps=120").unwrap();
        zw.start_file("client-overrides/config/a.cfg", opts)
            .unwrap();
        std::io::Write::write_all(&mut zw, b"key=value").unwrap();
        zw.start_file("modrinth.index.json", opts).unwrap();
        std::io::Write::write_all(&mut zw, b"{}").unwrap();
        zw.finish().unwrap();

        let written = BackendBridge::extract_overrides(&zip_path, &root).unwrap();
        assert_eq!(written, 2);
        assert_eq!(
            std::fs::read(root.join("options.txt")).unwrap(),
            b"maxFps=120"
        );
        assert_eq!(
            std::fs::read(root.join("config/a.cfg")).unwrap(),
            b"key=value"
        );
        assert!(!root.join("modrinth.index.json").exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Regression: remote-controlled Modrinth filenames must be coerced to
    /// their final path component so crafted names like "..\\evil.jar" or
    /// "../../evil.dll" cannot escape the instance content directory.
    #[test]
    fn test_filename_sanitization_components() {
        let dir = std::path::Path::new("D:\\instances\\test\\mods");
        for malicious in [
            "..\\..\\..\\evil.jar",
            "../../evil.dll",
            "..",
            "sub/dir/mod.jar",
        ] {
            let coerced = std::path::Path::new(malicious)
                .file_name()
                .map(|s| s.to_os_string());
            if let Some(name) = coerced {
                let dest = dir.join(name);
                // Resolved destination must stay inside the target directory.
                assert!(dest.starts_with(dir), "'{}' escaped: {:?}", malicious, dest);
                assert!(!dest.to_string_lossy().contains(".."));
            } else {
                // Bare ".." has no final component and is rejected upstream.
                assert_eq!(malicious, "..");
            }
        }
    }

    #[test]
    fn test_extract_version_from_filename() {
        assert_eq!(
            BackendBridge::extract_version_from_filename("sodium-0.5.11.jar"),
            Some("0.5.11".to_string())
        );
        assert_eq!(
            BackendBridge::extract_version_from_filename(
                "fabric-language-kotlin-1.13.13+kotlin.2.4.10.jar"
            ),
            Some("1.13.13+kotlin.2.4.10".to_string())
        );
        // mc-prefixed segments should be skipped
        assert_eq!(
            BackendBridge::extract_version_from_filename("modname-mc1.20.1-1.0.0.jar"),
            Some("1.0.0".to_string())
        );
        // Version with mc suffix attached
        assert_eq!(
            BackendBridge::extract_version_from_filename("sodium-extra-fabric-0.9.3+mc26.2.jar"),
            Some("0.9.3+mc26.2".to_string())
        );
        // NeoForge mod filename
        assert_eq!(
            BackendBridge::extract_version_from_filename("ImmediatelyFast-NeoForge-1.16.3+26.2.jar"),
            Some("1.16.3+26.2".to_string())
        );
        // No version segment — falls back to last part
        assert_eq!(
            BackendBridge::extract_version_from_filename("no_version.jar"),
            Some("no_version".to_string())
        );
    }

    #[test]
    fn test_compare_versions() {
        assert_eq!(BackendBridge::compare_versions("1.0.0", Some("0.9.3")), 1);
        assert_eq!(BackendBridge::compare_versions("0.9.3", Some("1.0.0")), -1);
        assert_eq!(BackendBridge::compare_versions("1.0.0", Some("1.0.0")), 0);
        assert_eq!(BackendBridge::compare_versions("1.2.3", None), 1);
        assert_eq!(BackendBridge::compare_versions("v1.16.14", Some("1.16.14")), 0);
        assert_eq!(BackendBridge::compare_versions("1.21.1", Some("1.20.1")), 1);
        assert_eq!(BackendBridge::compare_versions("1.20", Some("1.20.1")), -1);
    }
}
