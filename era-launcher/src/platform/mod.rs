#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Paths {
    pub data_local: std::path::PathBuf,
    pub config: std::path::PathBuf,
    pub instances: std::path::PathBuf,
    pub cache: std::path::PathBuf,
}

impl Paths {
    pub fn new() -> Self {
        let data_local = dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("EraLauncher");
        let config = data_local.join("config");
        let instances = data_local.join("instances");
        let cache = data_local.join("cache");
        Self {
            data_local,
            config,
            instances,
            cache,
        }
    }

    pub fn config_dir(&self) -> &std::path::Path {
        &self.config
    }
    pub fn instances_dir(&self) -> &std::path::Path {
        &self.instances
    }
}

impl Default for Paths {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
}

impl Platform {
    pub fn current() -> Self {
        let os = match std::env::consts::OS {
            "windows" => "windows",
            "macos" => "osx",
            _ => "linux",
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "arm64",
            other => other,
        };
        Self { os, arch }
    }
}
