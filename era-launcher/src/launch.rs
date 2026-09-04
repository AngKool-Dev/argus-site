use crate::downloads::DownloadManager;
use crate::minecraft::arguments::ArgumentBuilder;
use crate::minecraft::java::JavaManager;
use crate::minecraft::manifest::{ManifestClient, ManifestVersionInfo};
use crate::prelude::*;
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaunchRequest {
    pub instance_id: String,
    pub account_name: String,
    pub account_uuid: String,
    pub java_path: Option<String>,
    pub minecraft_dir: Option<String>,
    pub fresh: bool,
    pub memory: u32,
    pub game_version: String,
    pub loader: String,
    pub loader_version: Option<String>,
    pub optimization_profile: crate::minecraft::optimization::OptimizationProfile,
    pub custom_jvm_args: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaunchResult {
    pub success: bool,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub message: String,
    pub java_path: Option<String>,
}

pub struct LaunchEngine {
    manifest: ManifestClient,
}

impl LaunchEngine {
    pub fn new() -> Result<Self> {
        Ok(Self {
            manifest: ManifestClient::new()?,
        })
    }

    pub async fn launch(&self, req: &LaunchRequest, instances_dir: &Path) -> Result<LaunchResult> {
        let instance_dir = instances_dir.join(&req.instance_id);
        std::fs::create_dir_all(&instance_dir)?;

        self.emit_status("PREPARING", "Preparing instance...".to_string())
            .await?;

        self.emit_status(
            "PREPARING",
            format!("Resolving version {}...", req.game_version),
        )
        .await?;

        let version_info = self
            .manifest
            .get_version_info_by_id(&req.game_version)
            .await?;

        let mut loader_libs: Vec<PathBuf> = Vec::new();
        let loader_main_class: Option<String> = if req.loader == "fabric" {
            self.emit_status("DOWNLOADING", "Downloading Fabric loader...".to_string())
                .await?;
            self.download_fabric_loader(
                &req.game_version,
                req.loader_version.as_deref(),
                &instance_dir,
                &mut loader_libs,
            )
            .await?
        } else if req.loader == "forge" {
            self.emit_status("DOWNLOADING", "Downloading Forge libraries...".to_string())
                .await?;
            self.download_forge_libraries(
                &req.game_version,
                req.loader_version.as_deref(),
                &instance_dir,
                &mut loader_libs,
            )
            .await?
        } else {
            None
        };

        let java_path = if let Some(ref j) = req.java_path {
            let mut p = PathBuf::from(j);
            if p.is_dir() {
                let bin_name = if cfg!(windows) { "java.exe" } else { "java" };
                p = p.join("bin").join(bin_name);
            }
            p
        } else {
            let required = version_info
                .java_version
                .as_ref()
                .map(|jv| jv.major)
                .unwrap_or_else(|| JavaManager::required_for_minecraft(&req.game_version));
            match JavaManager::find_compatible(required) {
                Some(j) => j.path,
                None => {
                    self.emit_status(
                        "DOWNLOADING",
                        format!("Java {} not found. Downloading...", required),
                    )
                    .await?;
                    let install_dir = crate::platform::Paths::new().data_local;
                    let installed = crate::installer::install_java_runtime(install_dir, required)
                        .await
                        .map_err(|e| LauncherError::Java(format!("Java install failed: {}", e)))?;
                    installed.ok_or_else(|| {
                        LauncherError::Java("Java installation failed".to_string())
                    })?
                }
            }
        };

        let resolved_java_path = java_path.to_string_lossy().to_string();

        self.emit_status("DOWNLOADING", "Downloading client JAR...".to_string())
            .await?;
        let client_jar = self
            .download_client(&version_info, &instance_dir, req.fresh)
            .await?;

        self.emit_status("DOWNLOADING", "Resolving libraries...".to_string())
            .await?;
        let libs = self
            .download_libraries(&version_info, &instance_dir, req.fresh)
            .await?;

        self.emit_status("VERIFYING", "Extracting natives...".to_string())
            .await?;
        let natives_dir = self.extract_natives(&version_info, &instance_dir).await?;

        let game_dir = instance_dir.join("game");
        std::fs::create_dir_all(&game_dir)?;
        let assets_dir = instance_dir.join("assets");
        std::fs::create_dir_all(&assets_dir)?;

        self.emit_status("DOWNLOADING", "Downloading assets...".to_string())
            .await?;
        self.download_assets(&version_info, &assets_dir).await?;

        self.emit_status("VERIFYING", "Verifying files...".to_string())
            .await?;
        if !client_jar.exists() {
            return Err(LauncherError::Minecraft(
                "Client JAR missing after download".to_string(),
            ));
        }

        let classpath = self.build_classpath(
            &instance_dir,
            &client_jar,
            &libs,
            &natives_dir,
            &loader_libs,
            loader_main_class.is_some(),
        );

        for subdir in &["java", "jna", "lwjgl", "netty"] {
            let _ = std::fs::create_dir_all(natives_dir.join(subdir));
        }

        let (jvm_args, game_args, main_class) = self.build_args(
            &version_info,
            req,
            &game_dir,
            &assets_dir,
            &natives_dir,
            req.memory,
            loader_main_class.as_deref(),
            &classpath,
            req.optimization_profile,
            req.custom_jvm_args.clone(),
        );

        self.emit_status("LAUNCHING", "Starting Minecraft process...".to_string())
            .await?;

        let mut cmd = Command::new(&java_path);
        for arg in &jvm_args {
            cmd.arg(arg);
        }
        cmd.arg("-cp").arg(&classpath).arg(&main_class);
        for arg in &game_args {
            cmd.arg(arg);
        }
        eprintln!("=== LAUNCH DEBUG ===");
        eprintln!("Java path: {}", java_path.display());
        eprintln!("Java exists: {}", java_path.exists());
        eprintln!("Java is_file: {}", java_path.is_file());
        if let Ok(canonical) = std::fs::canonicalize(&java_path) {
            eprintln!("Java canonical: {}", canonical.display());
        }
        eprintln!("JVM args: {}", jvm_args.join(" "));
        eprintln!("Classpath: {}", classpath);
        eprintln!("Main class: {}", main_class);
        eprintln!("Game args: {}", game_args.join(" "));
        eprintln!("Game dir: {}", game_dir.display());
        eprintln!("Game dir exists: {}", game_dir.exists());
        if let Ok(meta) = std::fs::metadata(&game_dir) {
            eprintln!("Game dir is_dir: {}", meta.is_dir());
        }
        eprintln!("Current dir configured: true");
        eprintln!("stdin: null");
        eprintln!("stdout: piped");
        eprintln!("stderr: piped");
        eprintln!("Windows creation flags: default (none set)");

        let java_version_test = Command::new(&java_path).arg("-version").output();
        match java_version_test {
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!(
                    "Direct java -version test: status={}, output={}",
                    output.status, stderr
                );
            }
            Err(e) => {
                eprintln!("Direct java -version test FAILED: {}", e);
            }
        }
        eprintln!("===================");

        let _ = std::fs::write(
            std::env::temp_dir().join("era-launch-debug.txt"),
            format!(
                "Java: {}\nExists: {}\nIsFile: {}\nJVM: {}\nClasspathLen: {}\nGameDir: {}\n",
                java_path.display(),
                java_path.exists(),
                java_path.is_file(),
                jvm_args.join(" "),
                classpath.len(),
                game_dir.display()
            ),
        );
        cmd.current_dir(&game_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| LauncherError::Process(format!("Failed to spawn: {}", e)))?;
        let pid = child.id();

        self.emit_status(
            "RUNNING",
            format!("Minecraft {} launched (PID {})", req.game_version, pid),
        )
        .await?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        std::thread::spawn(move || {
            if let Some(stdout) = stdout {
                use std::io::BufRead;
                let mut reader = std::io::BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    let n = match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => 0,
                        Err(_) => break,
                    };
                    let _ = n;
                    if !line.is_empty() {}
                }
            }
            if let Some(stderr) = stderr {
                use std::io::BufRead;
                let mut reader = std::io::BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    let n = match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => 0,
                        Err(_) => break,
                    };
                    let _ = n;
                    if !line.is_empty() {}
                }
            }
        });

        Ok(LaunchResult {
            success: true,
            pid: Some(pid),
            exit_code: None,
            message: format!("Minecraft {} launched (PID {})", req.game_version, pid),
            java_path: Some(resolved_java_path),
        })
    }

    async fn emit_status(&self, _status: &str, _message: String) -> Result<()> {
        Ok(())
    }

    async fn download_client(
        &self,
        info: &ManifestVersionInfo,
        root: &Path,
        fresh: bool,
    ) -> Result<PathBuf> {
        let versions_dir = root.join("versions").join(&info.id);
        std::fs::create_dir_all(&versions_dir)?;
        let client_path = versions_dir.join(format!("{}.jar", info.id));
        if client_path.exists() && !fresh {
            return Ok(client_path);
        }
        if let Some(dl) = info.downloads.as_ref().and_then(|d| d.client.as_ref()) {
            let dm = self.download_with_progress(&info.id);
            dm.download(&dl.url, &client_path).await?;
        }
        Ok(client_path)
    }

    fn download_with_progress(&self, file_name_prefix: &str) -> DownloadManager {
        use std::sync::Arc;
        let _prefix = file_name_prefix.to_string();
        let cb: Arc<dyn Fn(crate::downloads::DownloadProgress) + Send + Sync> =
            Arc::new(move |_progress| {
                let _ = ();
            });
        DownloadManager::new().with_progress_callback(cb)
    }

    async fn download_libraries(
        &self,
        info: &ManifestVersionInfo,
        root: &Path,
        fresh: bool,
    ) -> Result<Vec<PathBuf>> {
        let libs_dir = root.join("libraries");
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
        let natives_keys = self.get_natives_keys(os, arch);

        let mut download_tasks: Vec<(String, PathBuf)> = Vec::new();
        let mut paths = Vec::new();
        for lib in &info.libraries {
            if !self.library_applies(&lib.rules) {
                continue;
            }
            let parts: Vec<&str> = lib.name.split(':').collect();
            if parts.len() >= 4 {
                let classifier = parts[3];
                if classifier.starts_with("natives-") {
                    let os_prefix = format!("natives-{}", os);
                    if !classifier.starts_with(&os_prefix) {
                        continue;
                    }
                    let suffix = classifier.strip_prefix(&format!("{}-", os_prefix));
                    let matches_arch = match suffix {
                        None => true,
                        Some(s) => {
                            if arch == "arm64" {
                                s == "arm64"
                            } else {
                                s != "x86" && s != "arm64"
                            }
                        }
                    };
                    if !matches_arch {
                        continue;
                    }
                }
            }
            let mut added = false;
            if let Some(ref downloads) = lib.downloads {
                if let Some(ref artifact) = downloads.artifact {
                    if !artifact.url.is_empty() {
                        let path = self.resolve_library_path(&lib.name, &libs_dir);
                        if fresh || !path.exists() {
                            download_tasks.push((artifact.url.clone(), path.clone()));
                        }
                        paths.push(path);
                        added = true;
                    }
                }
                if let Some(ref classifiers) = downloads.classifiers {
                    for key in &natives_keys {
                        if let Some(artifact) = classifiers.get(key) {
                            let class_path =
                                self.resolve_classifier_path(&lib.name, &libs_dir, key);
                            if fresh || !class_path.exists() {
                                download_tasks.push((artifact.url.clone(), class_path.clone()));
                            }
                            paths.push(class_path);
                            added = true;
                            break;
                        }
                    }
                }
            }
            if !added {
                paths.push(self.resolve_library_path(&lib.name, &libs_dir));
            }
        }

        let dm = DownloadManager::new();
        let stream = futures::stream::iter(download_tasks)
            .map(|(url, path)| {
                let dm = &dm;
                let url = url.clone();
                let path = path.clone();
                async move {
                    let result = dm.download(&url, &path).await;
                    result.is_ok()
                }
            })
            .buffer_unordered(8);
        tokio::pin!(stream);
        while let Some(_ok) = stream.next().await {}
        Ok(paths)
    }

    async fn extract_natives(&self, info: &ManifestVersionInfo, root: &Path) -> Result<PathBuf> {
        let natives_dir = root.join("natives");
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
        let natives_keys = self.get_natives_keys(os, arch);

        let libs_dir = root.join("libraries");
        for lib in &info.libraries {
            if !self.library_applies(&lib.rules) {
                continue;
            }
            let parts: Vec<&str> = lib.name.split(':').collect();
            if parts.len() >= 4 {
                // New format: classifier in name (e.g., "org.lwjgl:lwjgl-freetype:3.4.1:natives-windows")
                let classifier = parts[3];
                let os_prefix = format!("natives-{}", os);
                if !classifier.starts_with(&os_prefix) {
                    continue;
                }
                let suffix = classifier.strip_prefix(&format!("{}-", os_prefix));
                let matches_arch = match suffix {
                    None => true,
                    Some(s) => {
                        if arch == "arm64" {
                            s == "arm64"
                        } else {
                            s != "x86" && s != "arm64"
                        }
                    }
                };
                if !matches_arch {
                    continue;
                }
                let lib_path = self.resolve_library_path(&lib.name, &libs_dir);
                if lib_path.exists() {
                    self.extract_jar_natives(&lib_path, &natives_dir)?;
                }
            } else {
                // Old format: classifiers in downloads field
                let Some(ref downloads) = lib.downloads else {
                    continue;
                };
                let Some(ref classifiers) = downloads.classifiers else {
                    continue;
                };
                for key in &natives_keys {
                    if classifiers.get(key).is_some() {
                        let nat_path = self.resolve_classifier_path(&lib.name, &libs_dir, key);
                        if nat_path.exists() {
                            self.extract_jar_natives(&nat_path, &natives_dir)?;
                        }
                        break;
                    }
                }
            }
        }
        Ok(natives_dir)
    }

    fn extract_jar_natives(&self, jar_path: &Path, dest: &Path) -> Result<()> {
        let file = std::fs::File::open(jar_path)?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| LauncherError::Zip(e.to_string()))?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| LauncherError::Zip(e.to_string()))?;
            let entry_name = entry
                .enclosed_name()
                .ok_or_else(|| LauncherError::Zip("Invalid entry name".to_string()))?
                .to_owned();
            if entry_name.extension().is_some() {
                let out_path = dest.join(&entry_name);
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out_file = std::fs::File::create(&out_path)?;
                std::io::copy(&mut entry, &mut out_file)?;
            }
        }
        Ok(())
    }

    async fn download_assets(&self, info: &ManifestVersionInfo, assets_dir: &Path) -> Result<()> {
        let index_path = assets_dir
            .join("indexes")
            .join(format!("{}.json", info.asset_index.id));
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !index_path.exists() {
            let dm = self.download_with_progress(&info.asset_index.id);
            dm.download(&info.asset_index.url, &index_path).await?;
        }

        let index_content = std::fs::read_to_string(&index_path)?;
        let index: serde_json::Value = serde_json::from_str(&index_content)?;
        let objects = index
            .get("objects")
            .and_then(|o| o.as_object())
            .ok_or_else(|| LauncherError::Asset("Invalid asset index".to_string()))?;

        let objects_dir = assets_dir.join("objects");
        std::fs::create_dir_all(&objects_dir)?;

        let mut tasks: Vec<(String, PathBuf)> = Vec::new();
        for (name, obj) in objects {
            let hash = obj
                .get("hash")
                .and_then(|h| h.as_str())
                .ok_or_else(|| LauncherError::Asset(format!("Missing hash for asset {}", name)))?;
            let obj_path = objects_dir.join(&hash[..2]).join(hash);
            if !obj_path.exists() {
                let url = format!(
                    "https://resources.download.minecraft.net/{}/{}",
                    &hash[..2],
                    hash
                );
                tasks.push((url, obj_path));
            }
        }

        let dm = DownloadManager::new();
        let stream = futures::stream::iter(tasks)
            .map(|(url, path)| {
                let dm = &dm;
                let url = url.clone();
                let path = path.clone();
                async move {
                    let result = dm.download(&url, &path).await;
                    result.is_ok()
                }
            })
            .buffer_unordered(8);

        tokio::pin!(stream);
        while let Some(_ok) = stream.next().await {}
        Ok(())
    }

    fn get_natives_keys(&self, os: &str, arch: &str) -> Vec<String> {
        let mut keys = Vec::new();
        keys.push(format!("natives-{}-{}", os, arch));
        keys.push(format!("natives-{}", os));
        keys
    }

    #[allow(clippy::too_many_arguments)]
    fn build_args(
        &self,
        info: &ManifestVersionInfo,
        req: &LaunchRequest,
        game_dir: &Path,
        assets_dir: &Path,
        natives_dir: &Path,
        memory: u32,
        loader_main_class: Option<&str>,
        classpath: &str,
        optimization_profile: crate::minecraft::optimization::OptimizationProfile,
        custom_jvm_args: Vec<String>,
    ) -> (Vec<String>, Vec<String>, String) {
        let tokens = vec![
            ("auth_player_name".to_string(), req.account_name.clone()),
            ("auth_uuid".to_string(), req.account_uuid.clone()),
            ("auth_access_token".to_string(), "0".to_string()),
            ("clientid".to_string(), "0".to_string()),
            ("auth_xuid".to_string(), req.account_uuid.clone()),
            ("version_name".to_string(), info.id.clone()),
            ("version_type".to_string(), info.version_type.clone()),
            (
                "game_directory".to_string(),
                game_dir.to_string_lossy().to_string(),
            ),
            (
                "assets_root".to_string(),
                assets_dir.to_string_lossy().to_string(),
            ),
            ("assets_index_name".to_string(), info.asset_index.id.clone()),
            ("launcher_name".to_string(), "EraLauncher".to_string()),
                ("launcher_version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
            (
                "natives_directory".to_string(),
                natives_dir.to_string_lossy().to_string(),
            ),
            ("resolution_width".to_string(), "854".to_string()),
            ("resolution_height".to_string(), "480".to_string()),
            ("classpath".to_string(), classpath.to_string()),
        ];

        let profile_args = optimization_profile.jvm_args(memory);
        let mut jvm_args = Vec::new();
        jvm_args.extend(profile_args);
        jvm_args.push("-Duser.language=en".to_string());
        let mut game_args: Vec<String> = Vec::new();

        if let Some(ref args) = info.arguments {
            let features = std::collections::HashMap::new();
            if args.jvm.iter().any(|v| v.is_string()) || args.jvm.iter().any(|v| v.is_object()) {
                let parsed = ArgumentBuilder::collect_args(&args.jvm, &features);
                jvm_args.extend(parsed);
            }
            if args.game.iter().any(|v| v.is_string()) || args.game.iter().any(|v| v.is_object()) {
                let parsed = ArgumentBuilder::collect_args(&args.game, &features);
                game_args.extend(parsed);
            }
        }

        if game_args.is_empty() {
            game_args = vec![
                "--username".to_string(),
                "${auth_player_name}".to_string(),
                "--version".to_string(),
                "${version_name}".to_string(),
                "--gameDir".to_string(),
                "${game_directory}".to_string(),
                "--assetsDir".to_string(),
                "${assets_root}".to_string(),
            ];
        }

        let jvm_args = ArgumentBuilder::substitute_tokens(&jvm_args, &tokens);
        let game_args = ArgumentBuilder::substitute_tokens(&game_args, &tokens);
        let mut jvm_args = jvm_args;
        jvm_args.extend(custom_jvm_args);
        let main_class = loader_main_class
            .map(|s| s.to_string())
            .or_else(|| info.main_class.clone())
            .unwrap_or_else(|| "net.minecraft.client.main.Main".to_string());

        (jvm_args, game_args, main_class)
    }

    fn build_classpath(
        &self,
        _root: &Path,
        client_jar: &Path,
        libs: &[PathBuf],
        natives_dir: &Path,
        loader_libs: &[PathBuf],
        _has_loader: bool,
    ) -> String {
        let mut parts: Vec<PathBuf> = Vec::new();
        parts.extend(loader_libs.iter().cloned());
        parts.push(client_jar.to_path_buf());
        parts.extend(libs.iter().cloned());
        if natives_dir.exists() {
            parts.push(natives_dir.to_path_buf());
        }
        let sep = if cfg!(windows) { ";" } else { ":" };
        parts
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(sep)
    }

    fn resolve_library_path(&self, name: &str, base: &Path) -> PathBuf {
        let parts: Vec<&str> = name.split(':').collect();
        if parts.len() >= 3 {
            let group = parts[0].replace('.', "/");
            let artifact = parts[1];
            let version = parts[2];
            if parts.len() > 3 {
                let classifier = parts[3];
                base.join(group)
                    .join(artifact)
                    .join(version)
                    .join(format!("{}-{}-{}.jar", artifact, version, classifier))
            } else {
                base.join(group)
                    .join(artifact)
                    .join(version)
                    .join(format!("{}-{}.jar", artifact, version))
            }
        } else {
            base.join(name.replace(':', "/")).with_extension("jar")
        }
    }

    fn resolve_classifier_path(&self, name: &str, base: &Path, classifier: &str) -> PathBuf {
        let parts: Vec<&str> = name.split(':').collect();
        if parts.len() >= 3 {
            let group = parts[0].replace('.', "/");
            let artifact = parts[1];
            let version = parts[2];
            base.join(group)
                .join(artifact)
                .join(version)
                .join(format!("{}-{}-{}.jar", artifact, version, classifier))
        } else {
            base.join(name.replace(':', "/")).with_extension("jar")
        }
    }

    fn library_applies(&self, rules: &Option<serde_json::Value>) -> bool {
        let Some(rules) = rules else {
            return true;
        };
        let arr = match rules.as_array() {
            Some(a) => a,
            None => return true,
        };
        let mut allowed = false;
        let mut matched = false;
        for rule in arr {
            let obj = match rule.as_object() {
                Some(o) => o,
                None => continue,
            };
            let action = obj
                .get("action")
                .and_then(|a| a.as_str())
                .unwrap_or("allow");
            let os = obj.get("os").and_then(|o| o.as_object());
            let mut ok = true;
            if let Some(os) = os {
                if let Some(name) = os.get("name").and_then(|n| n.as_str()) {
                    let current = match std::env::consts::OS {
                        "windows" => "windows",
                        "macos" => "osx",
                        _ => "linux",
                    };
                    if current != name {
                        ok = false;
                    }
                }
            }
            matched = true;
            if ok {
                allowed = action == "allow";
            }
        }
        matched && allowed
    }

    async fn download_fabric_loader(
        &self,
        game_version: &str,
        loader_version: Option<&str>,
        instance_dir: &Path,
        loader_libs: &mut Vec<PathBuf>,
    ) -> Result<Option<String>> {
        let _ = game_version;
        let fabric_loader_version = if let Some(v) = loader_version {
            v.to_string()
        } else {
            let versions = self
                .get_fabric_loader_versions_from_meta()
                .await
                .unwrap_or_else(|_| vec!["0.16.14".to_string()]);
            versions
                .first()
                .cloned()
                .unwrap_or_else(|| "0.16.14".to_string())
        };

        let lib_dir = instance_dir.join("libraries");
        let loader_lib = lib_dir
            .join("net/fabricmc/fabric-loader")
            .join(&fabric_loader_version)
            .join(format!("fabric-loader-{}.jar", fabric_loader_version));
        if !loader_lib.exists() {
            let url = format!(
                "https://maven.fabricmc.net/net/fabricmc/fabric-loader/{}/fabric-loader-{}.jar",
                fabric_loader_version, fabric_loader_version
            );
            let dm = DownloadManager::new();
            dm.download(&url, &loader_lib).await?;
        }
        loader_libs.push(loader_lib.clone());

        let fabric_lib_url = format!(
            "https://maven.fabricmc.net/net/fabricmc/fabric-loader/{}/fabric-loader-{}.json",
            fabric_loader_version, fabric_loader_version
        );
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let json_resp = client.get(&fabric_lib_url).send().await?;
        if json_resp.status().is_success() {
            let json_data: serde_json::Value = json_resp.json().await?;
            let common_libs = json_data.get("libraries").and_then(|l| l.get("common"));
            if let Some(arr) = common_libs.and_then(|a| a.as_array()) {
                let dm = DownloadManager::new();
                for lib in arr {
                    if let Some(name) = lib.get("name").and_then(|n| n.as_str()) {
                        if let Some(url) = lib.get("url").and_then(|u| u.as_str()) {
                            let parts: Vec<&str> = name.split(':').collect();
                            if parts.len() >= 3 {
                                let group = parts[0].replace('.', "/");
                                let artifact = parts[1];
                                let version = parts[2];
                                let lib_path = lib_dir
                                    .join(&group)
                                    .join(artifact)
                                    .join(version)
                                    .join(format!("{}-{}.jar", artifact, version));
                                if !lib_path.exists() {
                                    let full_url = format!(
                                        "{}{}/{}/{}/{}-{}.jar",
                                        url, group, artifact, version, artifact, version
                                    );
                                    let _ = dm.download(&full_url, &lib_path).await;
                                }
                                if lib_path.exists() {
                                    loader_libs.push(lib_path);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(Some(
            "net.fabricmc.loader.impl.launch.knot.KnotClient".to_string(),
        ))
    }

    async fn download_forge_libraries(
        &self,
        game_version: &str,
        loader_version: Option<&str>,
        instance_dir: &Path,
        loader_libs: &mut Vec<PathBuf>,
    ) -> Result<Option<String>> {
        let lib_dir = instance_dir.join("libraries");

        let version_parts: Vec<&str> = game_version.split('.').collect();
        let major = version_parts.get(1).copied().unwrap_or("16");
        let major_num: u32 = major.parse().unwrap_or(16);

        if major_num <= 16 {
            let forge_version = loader_version
                .map(|v| v.to_string())
                .unwrap_or_else(|| {
                    if game_version == "1.16.5" {
                        "36.2.39".to_string()
                    } else if game_version == "1.15.2" {
                        "31.2.57".to_string()
                    } else {
                        "32.0.108".to_string()
                    }
                });

            let url = format!(
                "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/{}.jar",
                forge_version, forge_version
            );
            let dest = lib_dir
                .join("net/minecraftforge/forge")
                .join(&forge_version)
                .join(format!("forge-{}.jar", forge_version));
            if !dest.exists() {
                let dm = DownloadManager::new();
                dm.download(&url, &dest).await?;
            }
            loader_libs.push(dest);
            Ok(Some("net.minecraft.launchwrapper.Launch".to_string()))
        } else {
            Ok(None)
        }
    }

    async fn get_fabric_loader_versions_from_meta(&self) -> Result<Vec<String>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        let url = "https://meta.ichun.me/data/mc/mcVersions.json";
        let resp = client
            .get(url)
            .header("User-Agent", "EraLauncher/0.1.5")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(vec!["0.16.14".to_string()]);
        }
        let data: serde_json::Value = resp.json().await?;
        let loader_versions = data
            .get("loader")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut sorted = loader_versions;
        sorted.sort();
        sorted.reverse();
        Ok(sorted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_resolve_library_path_standard() {
        let engine = LaunchEngine::new().unwrap();
        let path =
            engine.resolve_library_path("com.mojang:brigadier:1.0.18", Path::new("/tmp/libs"));
        assert_eq!(
            path,
            PathBuf::from("/tmp/libs/com/mojang/brigadier/1.0.18/brigadier-1.0.18.jar")
        );
    }

    #[test]
    fn test_resolve_library_path_nested_group() {
        let engine = LaunchEngine::new().unwrap();
        let path = engine
            .resolve_library_path("org.jetbrains:annotations:1.190.0", Path::new("/tmp/libs"));
        assert_eq!(
            path,
            PathBuf::from("/tmp/libs/org/jetbrains/annotations/1.190.0/annotations-1.190.0.jar")
        );
    }

    #[test]
    fn test_resolve_library_path_with_classifier() {
        let engine = LaunchEngine::new().unwrap();
        let path = engine.resolve_library_path(
            "org.lwjgl:lwjgl-freetype:3.4.1:natives-windows",
            Path::new("/tmp/libs"),
        );
        assert_eq!(
            path,
            PathBuf::from(
                "/tmp/libs/org/lwjgl/lwjgl-freetype/3.4.1/lwjgl-freetype-3.4.1-natives-windows.jar"
            )
        );
    }

    #[test]
    fn test_resolve_classifier_path_windows() {
        let engine = LaunchEngine::new().unwrap();
        let path = engine.resolve_classifier_path(
            "net.minecraftforge:lwjgl:2.9.0",
            Path::new("/tmp/libs"),
            "natives-windows-x86_64",
        );
        assert_eq!(
            path,
            PathBuf::from(
                "/tmp/libs/net/minecraftforge/lwjgl/2.9.0/lwjgl-2.9.0-natives-windows-x86_64.jar"
            )
        );
    }

    #[test]
    fn test_resolve_library_path_fallback() {
        let engine = LaunchEngine::new().unwrap();
        let path = engine.resolve_library_path("invalid-name", Path::new("/tmp/libs"));
        assert_eq!(path, PathBuf::from("/tmp/libs/invalid-name.jar"));
    }

    #[test]
    fn test_library_applies_no_rules() {
        let engine = LaunchEngine::new().unwrap();
        assert!(engine.library_applies(&None));
    }

    #[test]
    fn test_library_applies_empty_rules() {
        let engine = LaunchEngine::new().unwrap();
        let rules = serde_json::Value::Array(vec![]);
        assert!(!engine.library_applies(&Some(rules)));
    }

    #[test]
    fn test_library_applies_always_allowed() {
        let engine = LaunchEngine::new().unwrap();
        let rules = serde_json::Value::Array(vec![serde_json::json!({"action": "allow"})]);
        assert!(engine.library_applies(&Some(rules)));
    }
}
