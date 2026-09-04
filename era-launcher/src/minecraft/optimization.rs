use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OptimizationProfile {
    Low,
    #[default]
    Mid,
    High,
    Custom,
}

impl OptimizationProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            OptimizationProfile::Low => "Low",
            OptimizationProfile::Mid => "Mid",
            OptimizationProfile::High => "High",
            OptimizationProfile::Custom => "Custom",
        }
    }

    pub fn all() -> &'static [OptimizationProfile] {
        &[
            OptimizationProfile::Low,
            OptimizationProfile::Mid,
            OptimizationProfile::High,
            OptimizationProfile::Custom,
        ]
    }

    pub fn jvm_args(&self, memory_mb: u32) -> Vec<String> {
        match self {
            OptimizationProfile::Low => vec![
                format!("-Xmx{}M", (memory_mb * 3 / 4).max(2048)),
                format!("-Xms{}M", (memory_mb / 8).clamp(512, 1024)),
                "-XX:+UseG1GC".to_string(),
                "-XX:+UnlockExperimentalVMOptions".to_string(),
                "-XX:G1NewSizePercent=20".to_string(),
                "-XX:G1ReservePercent=20".to_string(),
                "-XX:MaxGCPauseMillis=50".to_string(),
                "-XX:G1HeapRegionSize=32M".to_string(),
                "-XX:+UseStringDeduplication".to_string(),
                "-XX:+DisableExplicitGC".to_string(),
                "-XX:-OmitStackTraceInFastThrow".to_string(),
                "-XX:MaxDirectMemorySize=512M".to_string(),
                "-Dfml.ignoreInvalidMinecraftCertificates=true".to_string(),
                "-Dfml.ignorePatchDiscrepancies=true".to_string(),
            ],
            OptimizationProfile::Mid => vec![
                format!("-Xmx{}M", (memory_mb * 3 / 4).max(4096)),
                format!("-Xms{}M", (memory_mb / 4).clamp(1024, 2048)),
                "-XX:+UseG1GC".to_string(),
                "-XX:+UnlockExperimentalVMOptions".to_string(),
                "-XX:G1NewSizePercent=20".to_string(),
                "-XX:G1ReservePercent=20".to_string(),
                "-XX:MaxGCPauseMillis=50".to_string(),
                "-XX:G1HeapRegionSize=64M".to_string(),
                "-XX:+UseStringDeduplication".to_string(),
                "-XX:+DisableExplicitGC".to_string(),
                "-XX:-OmitStackTraceInFastThrow".to_string(),
            ],
            OptimizationProfile::High => vec![
                format!("-Xmx{}M", (memory_mb * 7 / 8).max(8192)),
                format!("-Xms{}M", (memory_mb / 4).clamp(2048, 4096)),
                "-XX:+UseG1GC".to_string(),
                "-XX:+UnlockExperimentalVMOptions".to_string(),
                "-XX:G1NewSizePercent=20".to_string(),
                "-XX:G1ReservePercent=20".to_string(),
                "-XX:MaxGCPauseMillis=50".to_string(),
                "-XX:G1HeapRegionSize=64M".to_string(),
                "-XX:+UseStringDeduplication".to_string(),
                "-XX:+DisableExplicitGC".to_string(),
                "-XX:-OmitStackTraceInFastThrow".to_string(),
                "-XX:+UseCompressedOops".to_string(),
                "-XX:+UseCompressedClassPointers".to_string(),
            ],
            OptimizationProfile::Custom => vec![],
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            OptimizationProfile::Low => "4GB RAM / Integrated GPU — balanced heap, smaller regions",
            OptimizationProfile::Mid => "8GB RAM / Dedicated GPU — standard G1 tuning",
            OptimizationProfile::High => "16GB+ RAM / High-end GPU — larger heap, compressed oops",
            OptimizationProfile::Custom => "No preset — uses only memory and manifest defaults",
        }
    }
}
