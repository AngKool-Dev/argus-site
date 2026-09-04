#![allow(dead_code)]
pub mod argus;
pub mod auth;
pub mod config;
pub mod downloads;
pub mod errors;
pub mod installer;
pub mod instances;
pub mod launch;
pub mod minecraft;
pub mod modrinth;
pub mod platform;
pub mod prelude;
pub mod servers;
pub mod crashes;
pub mod worlds;
pub mod versions;

use once_cell::sync::Lazy;
use std::sync::Mutex;

use crate::config::Config;
use crate::instances::InstanceManager;
use crate::launch::LaunchEngine;
use crate::minecraft::java::JavaManager;
use crate::minecraft::manifest::ManifestClient;
use crate::modrinth::{ModrinthClient, Version};
use crate::versions::{ScanResult, SystemScanner};

fn to_config_instance(i: &crate::instances::InstanceConfig) -> crate::config::InstanceConfig {
    crate::config::InstanceConfig {
        id: i.id.clone(),
        name: i.name.clone(),
        game_version: i.game_version.clone(),
        loader: i.loader.clone(),
        loader_version: i.loader_version.clone(),
        memory: i.memory,
        java: i.java.clone(),
        game_dir: i.game_dir.clone(),
        resolution_width: i.resolution_width,
        resolution_height: i.resolution_height,
        account_uuid: i.account_uuid.clone(),
        minecraft_dir: i.minecraft_dir.clone(),
        custom_jvm_args: i.custom_jvm_args.clone(),
    }
}

fn from_config_instance(i: &crate::config::InstanceConfig) -> crate::instances::InstanceConfig {
    crate::instances::InstanceConfig {
        id: i.id.clone(),
        name: i.name.clone(),
        game_version: i.game_version.clone(),
        loader: i.loader.clone(),
        loader_version: i.loader_version.clone(),
        memory: i.memory,
        java: i.java.clone(),
        game_dir: i.game_dir.clone(),
        resolution_width: i.resolution_width,
        resolution_height: i.resolution_height,
        account_uuid: i.account_uuid.clone(),
        minecraft_dir: i.minecraft_dir.clone(),
        custom_jvm_args: i.custom_jvm_args.clone(),
    }
}

/// CONFIG loads from disk on first access so persisted settings and instances
/// survive restarts. Previously this was `Config::default()` which silently
/// discarded everything saved by earlier sessions.
pub(crate) static CONFIG: Lazy<Mutex<Config>> =
    Lazy::new(|| Mutex::new(Config::load().unwrap_or_default()));

/// INSTANCE_MANAGER is hydrated from the loaded config so previously created
/// instances appear in both the GUI and the terminal UI after a restart.
pub(crate) static INSTANCE_MANAGER: Lazy<Mutex<InstanceManager>> = Lazy::new(|| {
    let cfg = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    let mut mgr = InstanceManager::new();
    for instance in &cfg.instances {
        mgr.add(from_config_instance(instance));
    }
    Mutex::new(mgr)
});

fn get_config() -> Config {
    CONFIG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn save_config(config: Config) -> anyhow::Result<()> {
    config.save().map_err(|e| anyhow::anyhow!(e))?;
    *CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = config;
    Ok(())
}

fn list_instances() -> Vec<crate::instances::InstanceConfig> {
    INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner()).list().to_vec()
}

fn create_instance(instance: crate::instances::InstanceConfig) -> crate::instances::InstanceConfig {
    let mut m = INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner());
    m.add(instance.clone());
    let mut config = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    config.instances.push(to_config_instance(&instance));
    let _ = config.save();
    drop(m);
    instance
}

fn delete_instance(id: String) -> bool {
    INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    let mut config = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    config.instances.retain(|i| i.id != id);
    let _ = config.save();
    true
}

fn update_instance(instance: crate::instances::InstanceConfig) -> bool {
    INSTANCE_MANAGER.lock().unwrap_or_else(|e| e.into_inner()).update(instance.clone());
    let mut config = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    let cfg_inst = to_config_instance(&instance);
    if let Some(existing) = config.instances.iter_mut().find(|i| i.id == instance.id) {
        *existing = cfg_inst;
    }
    let _ = config.save();
    true
}

fn scan_versions() -> Vec<ScanResult> {
    SystemScanner::new().scan().unwrap_or_default()
}

async fn get_versions() -> Vec<String> {
    let client = match ManifestClient::new() {
        Ok(c) => c,
        Err(_) => {
            return vec![
                "1.21.5".to_string(),
                "1.21.4".to_string(),
                "1.21.1".to_string(),
                "1.20.4".to_string(),
                "1.20.1".to_string(),
                "1.19.4".to_string(),
                "1.18.2".to_string(),
                "1.16.5".to_string(),
            ];
        }
    };
    client.get_all_versions().await.unwrap_or_else(|_| {
        vec![
            "1.21.5".to_string(),
            "1.21.4".to_string(),
            "1.21.1".to_string(),
            "1.20.4".to_string(),
            "1.20.1".to_string(),
            "1.19.4".to_string(),
            "1.18.2".to_string(),
            "1.16.5".to_string(),
        ]
    })
}

async fn get_fabric_loader_versions(_game_version: String) -> Vec<String> {
    let fabric_url = "https://meta.ichun.me/data/mc/mcVersions.json";
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let resp = http
        .get(fabric_url)
        .header("User-Agent", "EraLauncher/0.1.5")
        .send()
        .await;
    if let Ok(resp) = resp {
        if resp.status().is_success() {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                let mut versions: Vec<String> = data
                    .get("loader")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                versions.sort();
                versions.reverse();
                return versions;
            }
        }
    }
    vec![
        "0.16.14".to_string(),
        "0.16.10".to_string(),
        "0.16.9".to_string(),
        "0.15.11".to_string(),
    ]
}

async fn get_forge_versions(_game_version: String) -> Vec<String> {
    vec![
        "53.0.27".to_string(),
        "52.0.23".to_string(),
        "48.0.28".to_string(),
        "47.3.0".to_string(),
        "40.2.9".to_string(),
        "36.2.34".to_string(),
    ]
}

pub async fn launch_instance(
    req: crate::launch::LaunchRequest,
    instances_dir: String,
) -> anyhow::Result<crate::launch::LaunchResult> {
    let engine = LaunchEngine::new().map_err(|e| anyhow::anyhow!(e))?;
    let path = std::path::PathBuf::from(instances_dir);
    engine
        .launch(&req, &path)
        .await
        .map_err(|e| anyhow::anyhow!(e))
}

async fn search_modrinth(
    query: String,
    content_type: String,
    game_version: String,
    loader: String,
) -> anyhow::Result<Vec<crate::modrinth::Project>> {
    let client = crate::modrinth::ModrinthClient::new().map_err(|e| anyhow::anyhow!(e))?;
    let mut facets: Vec<String> = vec![format!("project_type:{}", content_type)];
    if !game_version.is_empty() {
        facets.push(format!("versions:{}", game_version));
    }
    if !loader.is_empty() {
        facets.push(format!("loaders:{}", loader));
    }
    let result = client
        .search(&query, 20, 0, &facets, Some("relevance"))
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(result.hits)
}

async fn get_mod_versions(project_id: String) -> anyhow::Result<Vec<Version>> {
    let client = ModrinthClient::new().map_err(|e| anyhow::anyhow!(e))?;
    client
        .get_project_versions(&project_id)
        .await
        .map_err(|e| anyhow::anyhow!(e))
}

async fn install_mod(
    _project_id: String,
    _version_id: String,
    file_url: String,
    file_name: String,
    instance_id: String,
    content_type: String,
    instances_dir: String,
) -> anyhow::Result<()> {
    let base = std::path::PathBuf::from(instances_dir).join(instance_id);
    let dest_dir = match content_type.as_str() {
        "modpack" => base.join("modpacks"),
        "resourcepack" => base.join("resourcepacks"),
        "shader" => base.join("shaderpacks"),
        _ => base.join("mods"),
    };
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(&file_name);
    let dm = crate::downloads::DownloadManager::new();
    dm.download(&file_url, &dest)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

fn get_java_installations() -> Vec<crate::minecraft::java::JavaInstallation> {
    JavaManager::detect_all()
}

fn get_instances_dir() -> String {
    crate::platform::Paths::new()
        .instances_dir()
        .to_string_lossy()
        .to_string()
}

fn get_installer_info() -> serde_json::Value {
    serde_json::json!({
        "install_dir": crate::platform::Paths::new().data_local.to_string_lossy().to_string(),
        "java_detected": {
            "required_major": 21u32,
            "found": crate::minecraft::java::JavaManager::find_compatible(21)
                .map(|j| j.version.as_ref().map(|v| v.major))
        },
        "java_installations": crate::minecraft::java::JavaManager::detect_all()
            .into_iter()
            .map(|j| serde_json::json!({
                "path": j.path.to_string_lossy().to_string(),
                "version": j.version.as_ref().map(|v| v.major)
            }))
            .collect::<Vec<_>>()
    })
}

fn prepare_instance(
    instance: crate::instances::InstanceConfig,
    instances_dir: String,
) -> anyhow::Result<()> {
    let base = std::path::PathBuf::from(instances_dir);
    instance
        .prepare_dirs(&base)
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

async fn install_java_runtime(java_version: u32) -> anyhow::Result<Option<String>> {
    let install_dir = crate::platform::Paths::new().data_local;
    let path = crate::installer::install_java_runtime(install_dir, java_version)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(path.map(|p| p.to_string_lossy().to_string()))
}
