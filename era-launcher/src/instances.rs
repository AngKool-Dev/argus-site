use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub id: String,
    pub name: String,
    pub game_version: String,
    pub loader: String,
    pub loader_version: Option<String>,
    pub memory: u32,
    pub java: Option<String>,
    pub game_dir: Option<String>,
    pub resolution_width: Option<u32>,
    pub resolution_height: Option<u32>,
    pub account_uuid: Option<String>,
    pub minecraft_dir: Option<String>,
    pub custom_jvm_args: Vec<String>,
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: "New Instance".to_string(),
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstanceManager {
    pub instances: Vec<InstanceConfig>,
}

impl InstanceManager {
    pub fn new() -> Self {
        Self {
            instances: Vec::new(),
        }
    }

    pub fn list(&self) -> &[InstanceConfig] {
        &self.instances
    }

    pub fn add(&mut self, instance: InstanceConfig) {
        self.instances.push(instance);
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let initial = self.instances.len();
        self.instances.retain(|i| i.id != id);
        self.instances.len() != initial
    }

    pub fn update(&mut self, instance: InstanceConfig) -> bool {
        if let Some(i) = self.instances.iter_mut().find(|i| i.id == instance.id) {
            *i = instance;
            true
        } else {
            false
        }
    }
}

impl InstanceConfig {
    pub fn instance_dir(&self, base: &Path) -> PathBuf {
        base.join(&self.id)
    }

    pub fn prepare_dirs(&self, base: &Path) -> crate::prelude::Result<()> {
        let dir = self.instance_dir(base);
        // NOTE: no "game" subdirectory — the instance root IS the game
        // directory (mods/, saves/, resourcepacks/ live here and --gameDir
        // points here).
        std::fs::create_dir_all(dir.join("libraries"))?;
        std::fs::create_dir_all(dir.join("natives"))?;
        std::fs::create_dir_all(dir.join("assets"))?;
        std::fs::create_dir_all(dir.join("mods"))?;
        std::fs::create_dir_all(dir.join("config"))?;
        std::fs::create_dir_all(dir.join("saves"))?;
        std::fs::create_dir_all(dir.join("resourcepacks"))?;
        std::fs::create_dir_all(dir.join("shaderpacks"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;
    use std::path::PathBuf;

    #[test]
    fn test_instance_default() {
        let inst = InstanceConfig::default();
        assert_eq!(inst.name, "New Instance");
        assert_eq!(inst.game_version, "1.21.1");
        assert_eq!(inst.loader, "vanilla");
        assert_eq!(inst.memory, 4096);
        assert!(!inst.id.is_empty());
    }

    #[test]
    fn test_instance_dir_path() {
        let inst = InstanceConfig {
            id: "test-instance".to_string(),
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
        let base = PathBuf::from("/instances");
        let dir = inst.instance_dir(&base);
        assert_eq!(dir, PathBuf::from("/instances/test-instance"));
    }

    #[test]
    fn test_prepare_dirs_creates_structure() {
        let tmp = temp_dir().join(format!(
            "era-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let inst = InstanceConfig {
            id: "test-prepare".to_string(),
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
        inst.prepare_dirs(&tmp).unwrap();
        let base = inst.instance_dir(&tmp);
        assert!(base.join("libraries").is_dir());
        assert!(base.join("natives").is_dir());
        assert!(base.join("assets").is_dir());
        assert!(base.join("mods").is_dir());
        assert!(base.join("config").is_dir());
        assert!(base.join("saves").is_dir());
        assert!(base.join("resourcepacks").is_dir());
        assert!(base.join("shaderpacks").is_dir());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_instance_manager_crud() {
        let mut mgr = InstanceManager::new();
        let inst = InstanceConfig::default();
        mgr.add(inst.clone());
        assert_eq!(mgr.list().len(), 1);
        let find = |mgr: &InstanceManager, id: &str| mgr.list().iter().any(|i| i.id == id);
        assert!(find(&mgr, &inst.id));
        assert!(mgr.remove(&inst.id));
        assert_eq!(mgr.list().len(), 0);
        assert!(!find(&mgr, &inst.id));
    }

    #[test]
    fn test_instance_manager_update() {
        let mut mgr = InstanceManager::new();
        let inst = InstanceConfig::default();
        mgr.add(inst.clone());
        let mut updated = inst.clone();
        updated.name = "Updated Name".to_string();
        assert!(mgr.update(updated));
        let found = mgr.list().iter().find(|i| i.id == inst.id).unwrap();
        assert_eq!(found.name, "Updated Name");
    }
}
