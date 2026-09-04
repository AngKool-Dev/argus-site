use crate::auth::Account;
use crate::minecraft::optimization::OptimizationProfile;
use crate::prelude::*;
use serde::Deserialize;

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub default_memory: u32,
    pub java_path: Option<String>,
    pub theme: String,
    pub language: String,
    pub optimization_profile: OptimizationProfile,
    pub custom_jvm_args: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_memory: 4096,
            java_path: None,
            theme: "tokyo-night".to_string(),
            language: "en".to_string(),
            optimization_profile: OptimizationProfile::Mid,
            custom_jvm_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1100,
            height: 750,
            maximized: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub settings: Settings,
    pub instances: Vec<InstanceConfig>,
    pub accounts: Vec<Account>,
    pub window: WindowConfig,
    pub default_account: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let paths = crate::platform::Paths::new();
        let config_path = paths.config_dir().join("config.json");
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            serde_json::from_str(&content).map_err(Into::into)
        } else {
            Ok(Config::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let paths = crate::platform::Paths::new();
        std::fs::create_dir_all(paths.config_dir())?;
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(paths.config_dir().join("config.json"), content)?;
        Ok(())
    }

    pub fn add_instance(&mut self, instance: InstanceConfig) -> Result<()> {
        if self.instances.iter().any(|i| i.id == instance.id) {
            return Err(LauncherError::Instance(
                "Instance already exists".to_string(),
            ));
        }
        self.instances.push(instance);
        self.save()
    }

    pub fn remove_instance(&mut self, id: &str) -> Result<()> {
        let initial = self.instances.len();
        self.instances.retain(|i| i.id != id);
        if self.instances.len() == initial {
            return Err(LauncherError::Instance("Instance not found".to_string()));
        }
        self.save()
    }

    pub fn update_instance(&mut self, instance: InstanceConfig) -> Result<()> {
        if let Some(i) = self.instances.iter_mut().find(|i| i.id == instance.id) {
            *i = instance;
            self.save()
        } else {
            Err(LauncherError::Instance("Instance not found".to_string()))
        }
    }

    pub fn add_account(&mut self, account: Account) -> Result<()> {
        if self.accounts.iter().any(|a| a.uuid == account.uuid) {
            return Err(LauncherError::Config("Account already exists".to_string()));
        }
        self.accounts.push(account);
        self.save()
    }

    pub fn remove_account(&mut self, uuid: &str) -> Result<()> {
        let initial = self.accounts.len();
        self.accounts.retain(|a| a.uuid != uuid);
        if self.accounts.len() == initial {
            return Err(LauncherError::Config("Account not found".to_string()));
        }
        self.save()
    }
}
