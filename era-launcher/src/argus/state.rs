//! AppState — holds the ARGUS application state including navigation,
//! runtime state, and command interface.

use crate::argus::update::UpdateCheckResult;
use crate::instances::InstanceConfig;
use crate::minecraft::java::JavaInstallation;
use crate::minecraft::optimization::OptimizationProfile;
use crate::modrinth::Project;
use crate::servers::{PingInfo, ServerEntry};
use crate::versions::ScanResult;
use std::collections::VecDeque;
use std::path::PathBuf;

/// Navigation sections in ARGUS
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Home,
    Discover,
    Instances,
    Mods,
    Worlds,
    Servers,
    Logs,
    Crashes,
    Screenshots,
    Settings,
}

/// Sub-tabs within the Discover section
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[derive(Default)]
pub enum DiscoverTab {
    #[default]
    Mods,
    Modpacks,
    Shaders,
    ResourcePacks,
}

impl DiscoverTab {
    pub fn label(&self) -> &'static str {
        match self {
            DiscoverTab::Mods => "Mods",
            DiscoverTab::Modpacks => "Modpacks",
            DiscoverTab::Shaders => "Shaders",
            DiscoverTab::ResourcePacks => "Resource Packs",
        }
    }

    pub fn all() -> &'static [DiscoverTab] {
        &[
            DiscoverTab::Mods,
            DiscoverTab::Modpacks,
            DiscoverTab::Shaders,
            DiscoverTab::ResourcePacks,
        ]
    }
}

impl Section {
    pub fn label(&self) -> &'static str {
        match self {
            Section::Home => "HOME",
            Section::Discover => "DISCOVER",
            Section::Instances => "INSTANCES",
            Section::Mods => "MODS",
            Section::Worlds => "WORLDS",
            Section::Servers => "SERVERS",
            Section::Logs => "LOGS",
            Section::Crashes => "CRASHES",
            Section::Screenshots => "SCREENSHOTS",
            Section::Settings => "SETTINGS",
        }
    }

    pub fn all() -> &'static [Section] {
        &[
            Section::Home,
            Section::Discover,
            Section::Instances,
            Section::Mods,
            Section::Worlds,
            Section::Servers,
            Section::Logs,
            Section::Crashes,
            Section::Screenshots,
            Section::Settings,
        ]
    }
}

/// Navigation state (alias for Section for the navigation model)
pub type NavigationSection = Section;

/// Runtime states that the Minecraft process can be in
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error(String),
}

impl RuntimeState {
    pub fn label(&self) -> &'static str {
        match self {
            RuntimeState::Stopped => "STOPPED",
            RuntimeState::Starting => "STARTING",
            RuntimeState::Running => "RUNNING",
            RuntimeState::Stopping => "STOPPING",
            RuntimeState::Error(_) => "ERROR",
        }
    }

    pub fn status_indicator(&self) -> &'static str {
        match self {
            RuntimeState::Stopped => " ",
            RuntimeState::Starting => "↻",
            RuntimeState::Running => "●",
            RuntimeState::Stopping => "↺",
            RuntimeState::Error(_) => "✗",
        }
    }
}

/// Log entry for the terminal log viewer
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub source: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
}

impl LogLevel {
    pub fn label(&self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Debug => "DEBUG",
        }
    }
}

/// Current settings edit mode (when user is editing a setting value)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsEditMode {
    None,
    MemorySelector,
    JavaSelector,
    ThemeSelector,
    LanguageInfo,
    OptimizationSelector,
    CustomJvmEditor,
}

impl SettingsEditMode {
    /// Whether the app is currently in a settings editor that needs
    /// special key handling (Up/Down to navigate selectors, etc.)
    pub fn should_use_edit_keys(&self) -> bool {
        *self != SettingsEditMode::None
    }
}

/// A selectable memory preset for the memory selector
#[derive(Debug, Clone, Copy)]
pub struct MemoryPreset {
    pub mb: u32,
}

/// Memory presets available for selection
pub const MEMORY_PRESETS: &[u32] = &[2048, 4096, 6144, 8192, 12288, 16384];

/// Which DISCOVER sub-pane currently owns ↑/↓ navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiscoverPane {
    /// Category selector (and search field) — where you land on entry
    #[default]
    Categories,
    /// Result list — ESC returns to Categories, ESC again to the main tabs
    Results,
}

/// A project queued for install while the user picks an exact build in the
/// per-mod version chooser overlay.
#[derive(Debug, Clone)]
pub struct PendingInstall {
    pub project_id: String,
    pub title: String,
    pub content_type: String,
    pub instance_id: Option<String>,
    /// (modrinth version id, display label) — newest first, releases first
    pub rows: Vec<(String, String)>,
}

/// A piece of content (mod/resource pack/shader) installed in an instance.
#[derive(Debug, Clone)]
pub struct InstalledContent {
    pub name: String,
    /// "MOD" | "RESOURCE PACK" | "SHADER"
    pub kind: &'static str,
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// An installed content item that has a newer version available on Modrinth.
#[derive(Debug, Clone)]
pub struct UpdatableMod {
    pub project_id: String,
    pub title: String,
    pub installed_version: String,
    pub latest_version: String,
    pub latest_version_id: String,
    pub content_type: String,
    pub filename: String,
}

/// Parsed summary of a JVM crash report (`hs_err_pid*.log`).
#[derive(Debug, Clone)]
pub struct CrashReport {
    pub path: PathBuf,
    pub timestamp: String,
    pub exception: String,
    pub thread: String,
    pub jvm_version: String,
    pub summary: String,
}

/// A screenshot on disk, surfaced in the SCREENSHOTS section.
#[derive(Debug, Clone)]
pub struct ScreenshotEntry {
    pub path: PathBuf,
    pub mtime: String,
    pub size_bytes: u64,
}

/// A diagnostic hint produced by `crashes::diagnose`.
#[derive(Debug, Clone)]
pub enum DiagnosticHint {
    MissingMod(String),
    ModVersionMismatch { mod_id: String, expected: String, got: String },
    NoClassDefFound(String),
    InsufficientMemory,
    JavaVersion(String),
    Unknown,
}

impl DiagnosticHint {
    pub fn label(&self) -> String {
        match self {
            DiagnosticHint::MissingMod(cls) => format!("Missing mod — class {}", cls),
            DiagnosticHint::ModVersionMismatch { mod_id, .. } => {
                format!("Mod version mismatch on {}", mod_id)
            }
            DiagnosticHint::NoClassDefFound(cls) => {
                format!("NoClassDefFoundError on {}", cls)
            }
            DiagnosticHint::InsufficientMemory => "Out of memory — raise RAM in Settings".to_string(),
            DiagnosticHint::JavaVersion(v) => format!("Java version mismatch — got {}", v),
            DiagnosticHint::Unknown => "Unknown crash".to_string(),
        }
    }
}

/// Loader choices offered when creating an instance: (id, description).
pub const CREATE_LOADERS: &[(&str, &str)] = &[
    ("vanilla", "Official Minecraft — no mods"),
    ("fabric", "Lightweight, fast mod loading"),
    ("quilt", "Fabric-compatible, community-driven"),
    ("forge", "Classic modding (not supported here yet)"),
];

/// The main application state for ARGUS
pub struct AppState {
    /// Currently active navigation section
    pub current_section: Section,
    /// Minecraft runtime state
    pub runtime_state: RuntimeState,
    /// Currently selected instance (if any)
    pub selected_instance: Option<InstanceConfig>,
    /// All instances from the backend
    pub instances: Vec<InstanceConfig>,
    /// Available Java installations
    pub java_installations: Vec<JavaInstallation>,
    /// Available Minecraft versions
    pub versions: Vec<String>,
    /// Available Fabric loader versions
    pub fabric_versions: Vec<String>,
    /// Available Forge versions
    pub forge_versions: Vec<String>,
    /// Modrinth search results (for DISCOVER/MODS sections)
    pub modrinth_results: Vec<Project>,
    /// Text typed into the DISCOVER search bar
    pub discover_search: String,
    /// Whether the DISCOVER search bar is capturing keystrokes
    pub search_mode: bool,
    /// Whether the keyboard-shortcuts help overlay is shown
    pub help_overlay: bool,
    /// Whether the create-instance loader picker overlay is open
    pub loader_selector_open: bool,
    /// Selected row inside the loader picker
    pub loader_selector_index: usize,
    /// Saved accounts (id, username) for the selector overlay
    pub accounts: Vec<(String, String)>,
    /// Username of the active launch account
    pub active_account_name: Option<String>,
    /// Whether the account picker overlay is open
    pub account_selector_open: bool,
    /// Selected row inside the account picker (last row = create new)
    pub account_selector_index: usize,
    /// Whether the "type new account name" input is capturing keys
    pub account_input_mode: bool,
    /// Buffer for the new-account-name input
    pub account_input: String,
    /// Whether the game-version picker overlay is open (create flow step 2)
    pub version_selector_open: bool,
    /// Selected row in the version picker
    pub version_selector_index: usize,
    /// Loader chosen before the version step
    pub pending_version_loader: Option<String>,
    /// Live filter text typed inside the version picker
    pub version_filter: String,
    /// Open per-mod install version chooser state
    pub pending_install: Option<PendingInstall>,
    /// Selected row in the install version chooser
    pub install_version_index: usize,
    /// Last focused target id per section, restored when revisiting
    pub focus_memory: Vec<(Section, String)>,
    /// Incremented every loop iteration; drives the loading spinner
    pub tick: u64,
    /// Scroll offset for the LOGS list
    pub log_scroll: usize,
    /// How many DISCOVER results were hidden because already installed
    pub discover_hidden_count: usize,
    /// Which DISCOVER pane owns ↑/↓ navigation right now
    pub discover_pane: crate::argus::state::DiscoverPane,
    /// Minecraft version Discover results are scoped to (for display)
    pub discover_game_version: String,
    /// Per-result "already installed in selected instance" flags, parallel
    /// to modrinth_results (drives hiding + the ✓ badge)
    pub result_installed: Vec<bool>,
    /// Whether hidden-installed results are currently revealed (toggle: i)
    pub show_installed_discover: bool,
    /// Instance ID the current modrinth_results were fetched for — when the
    /// user selects a different instance, the list is stale and must be
    /// re-scoped (version facets + install-hidden set belong to the old one)
    pub discover_scoped_instance: Option<String>,
    /// Content installed in the selected instance (MODS section)
    pub installed_content: Vec<InstalledContent>,
    /// Mods/resource packs/shaders with newer versions available
    pub updatable_mods: Vec<UpdatableMod>,
    /// JVM crash reports found for the selected instance
    pub crash_reports: Vec<CrashReport>,
    /// Diagnostic hints derived from the selected crash report
    pub crash_diagnostics: Vec<DiagnosticHint>,
    /// Worlds found in the selected instance's saves directory
    pub worlds: Vec<String>,
    /// Screenshots for the selected instance (vanilla `screenshots/` dir)
    pub screenshots: Vec<ScreenshotEntry>,
    /// User-added multiplayer servers
    pub servers: Vec<ServerEntry>,
    /// Most recent ping info per server, indexed by server id
    pub server_pings: std::collections::HashMap<String, PingInfo>,
    /// Whether the "Add server" overlay is open
    pub server_add_open: bool,
    /// Buffer for new-server name (overlay)
    pub server_add_name: String,
    /// Buffer for new-server address (overlay)
    pub server_add_address: String,
    /// True while a ping is in flight (so the button shows "Pinging…")
    pub server_pinging: bool,
    /// Whether the live log tail view is currently shown (Shift+L toggle)
    pub live_log_view: bool,
    /// Cached path to the live log file (set when Minecraft starts)
    pub live_log_path: Option<PathBuf>,
    /// System scan results
    pub scan_results: Vec<ScanResult>,
    /// Log entries
    pub logs: VecDeque<LogEntry>,
    /// Command history
    pub command_history: VecDeque<String>,
    /// Currently typed command in the prompt
    pub command_input: String,
    /// Command prompt position in history (0 = current, 1+ = going back)
    pub history_position: usize,
    /// Whether the command prompt is focused
    pub command_prompt_active: bool,
    /// Set when the app should exit the main loop (q / Ctrl+C / `quit`)
    pub should_quit: bool,
    /// Status message (for showing errors, info, etc.)
    pub status_message: Option<(String, std::time::Instant)>,
    /// Last error message
    pub error_message: Option<String>,
    /// Whether the UI should show a confirmation dialog
    pub confirm_dialog: Option<ConfirmDialog>,
    /// Instances directory path
    pub instances_dir: Option<String>,
    /// Current sub-tab within the Discover section
    pub discover_tab: DiscoverTab,
    /// Selected account
    pub selected_account: Option<String>,
    /// Loading flag
    pub loading: bool,
    /// Loading message
    pub loading_message: Option<String>,
    /// Settings edit mode (None = normal settings navigation)
    pub settings_edit_mode: SettingsEditMode,
    /// Currently selected index in settings editors
    pub settings_edit_index: usize,
    /// Theme options
    pub theme_options: Vec<String>,
    /// Latest update check result
    pub update_check: UpdateCheckResult,
    /// When the last update check was performed
    pub last_update_check: Option<std::time::Instant>,
    /// Receiver for background mod update check results
    pub mod_update_rx: Option<std::sync::mpsc::Receiver<Vec<crate::argus::state::UpdatableMod>>>,
    /// Version this binary was built from
    pub current_version: &'static str,
    /// Selected optimization profile for JVM args
    pub optimization_profile: OptimizationProfile,
    /// Temporary input buffer for custom JVM args editor
    pub custom_jvm_input: String,
}

/// A confirmation dialog
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub confirm_text: String,
    pub cancel_text: String,
}

impl AppState {
    /// Indices into modrinth_results that should appear in the list:
    /// installed ones only when the reveal toggle is on.
    pub fn visible_project_indices(&self) -> Vec<usize> {
        self.modrinth_results
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                self.show_installed_discover
                    || !self.result_installed.get(*i).copied().unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect()
    }
    pub fn new() -> Self {
        Self {
            current_section: Section::Home,
            runtime_state: RuntimeState::Stopped,
            selected_instance: None,
            instances: Vec::new(),
            java_installations: Vec::new(),
            versions: Vec::new(),
            fabric_versions: Vec::new(),
            forge_versions: Vec::new(),
            modrinth_results: Vec::new(),
            discover_search: String::new(),
            search_mode: false,
            help_overlay: false,
            loader_selector_open: false,
            loader_selector_index: 0,
            accounts: Vec::new(),
            active_account_name: None,
            account_selector_open: false,
            account_selector_index: 0,
            account_input_mode: false,
            account_input: String::new(),
            version_selector_open: false,
            version_selector_index: 0,
            pending_version_loader: None,
            version_filter: String::new(),
            pending_install: None,
            install_version_index: 0,
            focus_memory: Vec::new(),
            tick: 0,
            log_scroll: 0,
            discover_hidden_count: 0,
            discover_pane: DiscoverPane::Categories,
            discover_game_version: String::new(),
            discover_scoped_instance: None,
            result_installed: Vec::new(),
            show_installed_discover: false,
            installed_content: Vec::new(),
            updatable_mods: Vec::new(),
            crash_reports: Vec::new(),
            crash_diagnostics: Vec::new(),
            worlds: Vec::new(),
            screenshots: Vec::new(),
            servers: Vec::new(),
            server_pings: std::collections::HashMap::new(),
            server_add_open: false,
            server_add_name: String::new(),
            server_add_address: String::new(),
            server_pinging: false,
            live_log_view: false,
            live_log_path: None,
            scan_results: Vec::new(),
            logs: VecDeque::with_capacity(1000),
            command_history: VecDeque::with_capacity(100),
            command_input: String::new(),
            history_position: 0,
            command_prompt_active: false,
            should_quit: false,
            status_message: None,
            error_message: None,
            confirm_dialog: None,
            instances_dir: None,
            discover_tab: DiscoverTab::default(),
            selected_account: None,
            loading: false,
            loading_message: None,
            settings_edit_mode: SettingsEditMode::None,
            settings_edit_index: 0,
            theme_options: vec![
                "dark".to_string(),
                "light".to_string(),
                "system".to_string(),
                "dracula".to_string(),
                "tokyo-night".to_string(),
            ],
            update_check: UpdateCheckResult::UpToDate,
            last_update_check: None,
            mod_update_rx: None,
            current_version: env!("CARGO_PKG_VERSION"),
            optimization_profile: OptimizationProfile::Mid,
            custom_jvm_input: String::new(),
        }
    }

    /// Numeric semver-ish comparison of release tags against the running
    /// version. Tolerates a leading `v`, missing components, and junk
    /// components (treated as 0). Pre-release suffixes like `-beta` make a
    /// component unparseable and therefore 0, so they never trigger the nag.
    pub fn is_newer_version(latest: &str, current: &str) -> bool {
        let parse = |s: &str| -> Vec<u64> {
            s.trim()
                .trim_start_matches('v')
                .split('.')
                .map(|p| p.parse::<u64>().unwrap_or(0))
                .collect()
        };
        let l = parse(latest);
        let c = parse(current);
        for i in 0..l.len().max(c.len()) {
            let lv = l.get(i).copied().unwrap_or(0);
            let cv = c.get(i).copied().unwrap_or(0);
            if lv != cv {
                return lv > cv;
            }
        }
        false
    }

    /// Add a log entry
    pub fn log(&mut self, level: LogLevel, source: &str, message: &str) {
        self.logs.push_back(LogEntry {
            timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
            level,
            source: source.to_string(),
            message: message.to_string(),
        });
        while self.logs.len() > 1000 {
            self.logs.pop_front();
        }
    }

    /// Add a command to history
    pub fn add_to_history(&mut self, command: &str) {
        if !command.trim().is_empty() {
            self.command_history.push_back(command.trim().to_string());
            while self.command_history.len() > 100 {
                self.command_history.pop_front();
            }
        }
        self.history_position = 0;
        self.command_input.clear();
    }

    /// Get the previous command from history
    pub fn previous_command(&mut self) {
        if self.command_history.is_empty() {
            return;
        }
        self.history_position = (self.history_position + 1).min(self.command_history.len());
        if let Some(cmd) = self
            .command_history
            .iter()
            .rev()
            .nth(self.history_position - 1)
        {
            self.command_input = cmd.clone();
        }
    }

    /// Get the next command from history
    pub fn next_command(&mut self) {
        if self.history_position > 0 {
            self.history_position -= 1;
            if self.history_position == 0 {
                self.command_input.clear();
            } else if let Some(cmd) = self
                .command_history
                .iter()
                .rev()
                .nth(self.history_position - 1)
            {
                self.command_input = cmd.clone();
            }
        }
    }

    /// Set a status message (with timeout for clearing)
    pub fn set_status(&mut self, message: String) {
        self.status_message = Some((message, std::time::Instant::now()));
    }

    /// Set an error message
    pub fn set_error(&mut self, message: String) {
        self.error_message = Some(message.clone());
        self.log(LogLevel::Error, "ARGUS", &format!("Error: {}", message));
    }

    /// Clear the error message
    pub fn clear_error(&mut self) {
        self.error_message = None;
    }

    /// Set loading state
    pub fn set_loading(&mut self, loading: bool, message: Option<String>) {
        self.loading = loading;
        self.loading_message = message;
    }

    /// Navigate to a section
    pub fn navigate_to(&mut self, section: Section) {
        self.current_section = section;
        self.log(
            LogLevel::Info,
            "ARGUS",
            &format!("Navigated to {}", section.label()),
        );
    }

    /// Select an instance by index
    pub fn select_instance(&mut self, index: usize) {
        if index < self.instances.len() {
            self.selected_instance = Some(self.instances[index].clone());
        }
    }

    /// Get the list of all sections for navigation
    pub fn sections() -> Vec<Section> {
        vec![
            Section::Home,
            Section::Discover,
            Section::Instances,
            Section::Mods,
            Section::Worlds,
            Section::Servers,
            Section::Logs,
            Section::Crashes,
            Section::Screenshots,
            Section::Settings,
        ]
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_version() {
        assert!(AppState::is_newer_version("v0.1.2", "0.1.1"));
        assert!(AppState::is_newer_version("0.2.0", "v0.1.9"));
        assert!(AppState::is_newer_version("1.0", "0.9.9"));
        assert!(AppState::is_newer_version("v1.0.0", "0.99.99"));
        assert!(!AppState::is_newer_version("v0.1.1", "0.1.1"));
        assert!(!AppState::is_newer_version("0.1.0", "0.1.1"));
        assert!(!AppState::is_newer_version("0.1", "0.1.1"));
        assert!(!AppState::is_newer_version("", "0.1.1"));
        assert!(!AppState::is_newer_version("garbage", "0.1.1"));
    }

    #[test]
    fn test_section_labels() {
        assert_eq!(Section::Home.label(), "HOME");
        assert_eq!(Section::Discover.label(), "DISCOVER");
        assert_eq!(Section::Instances.label(), "INSTANCES");
        assert_eq!(Section::Mods.label(), "MODS");
        assert_eq!(Section::Worlds.label(), "WORLDS");
        assert_eq!(Section::Logs.label(), "LOGS");
        assert_eq!(Section::Settings.label(), "SETTINGS");
    }

    #[test]
    fn test_runtime_state_indicators() {
        assert_eq!(RuntimeState::Stopped.status_indicator(), " ");
        assert_eq!(RuntimeState::Starting.status_indicator(), "↻");
        assert_eq!(RuntimeState::Running.status_indicator(), "●");
        assert_eq!(RuntimeState::Stopping.status_indicator(), "↺");
        assert_eq!(
            RuntimeState::Error("test".to_string()).status_indicator(),
            "✗"
        );
    }

    #[test]
    fn test_command_history() {
        let mut state = AppState::new();
        state.add_to_history("launch");
        state.add_to_history("status");
        assert_eq!(state.command_history.len(), 2);
        assert_eq!(state.history_position, 0);
    }

    #[test]
    fn test_command_history_previous_next() {
        let mut state = AppState::new();
        state.add_to_history("first");
        state.add_to_history("second");
        state.add_to_history("third");

        // Go back
        state.previous_command();
        assert_eq!(state.command_input, "third");
        assert_eq!(state.history_position, 1);

        state.previous_command();
        assert_eq!(state.command_input, "second");
        assert_eq!(state.history_position, 2);

        state.previous_command();
        assert_eq!(state.command_input, "first");
        assert_eq!(state.history_position, 3);

        // Can't go further back
        state.previous_command();
        assert_eq!(state.command_input, "first");
        assert_eq!(state.history_position, 3);

        // Go forward
        state.next_command();
        assert_eq!(state.command_input, "second");
        assert_eq!(state.history_position, 2);

        state.next_command();
        assert_eq!(state.command_input, "third");
        assert_eq!(state.history_position, 1);

        state.next_command();
        assert!(state.command_input.is_empty());
        assert_eq!(state.history_position, 0);
    }

    #[test]
    fn test_log_entries() {
        let mut state = AppState::new();
        state.log(LogLevel::Info, "TestSource", "Test message");
        assert_eq!(state.logs.len(), 1);
        let entry = state.logs.back().unwrap();
        assert_eq!(entry.level, LogLevel::Info);
        assert_eq!(entry.source, "TestSource");
        assert_eq!(entry.message, "Test message");
    }

    #[test]
    fn test_log_truncation() {
        let mut state = AppState::new();
        for i in 0..1100 {
            state.log(LogLevel::Info, "Test", &format!("Message {}", i));
        }
        assert_eq!(state.logs.len(), 1000);
    }

    #[test]
    fn test_set_error() {
        let mut state = AppState::new();
        state.set_error("Test error".to_string());
        assert_eq!(state.error_message, Some("Test error".to_string()));
        state.clear_error();
        assert_eq!(state.error_message, None);
    }

    #[test]
    fn test_select_instance() {
        let mut state = AppState::new();
        state.instances.push(InstanceConfig {
            id: "test-id".to_string(),
            name: "Test Instance".to_string(),
            game_version: "1.21.1".to_string(),
            loader: "fabric".to_string(),
            loader_version: Some("0.16.14".to_string()),
            memory: 4096,
            java: None,
            game_dir: None,
            resolution_width: None,
            resolution_height: None,
            account_uuid: None,
            minecraft_dir: None,
            custom_jvm_args: Vec::new(),
        });
        state.select_instance(0);
        assert!(state.selected_instance.is_some());
        assert_eq!(
            state.selected_instance.as_ref().unwrap().name,
            "Test Instance"
        );
    }

    #[test]
    fn test_empty_command_history_no_panic() {
        let mut state = AppState::new();
        state.previous_command();
        state.next_command();
        assert!(state.command_input.is_empty());
    }

    #[test]
    fn test_add_empty_to_history() {
        let mut state = AppState::new();
        state.add_to_history("");
        state.add_to_history("   ");
        assert_eq!(state.command_history.len(), 0);
        state.add_to_history("valid");
        assert_eq!(state.command_history.len(), 1);
    }

    #[test]
    fn test_sections_count() {
        let sections = AppState::sections();
        assert_eq!(sections.len(), 10);
    }

    #[test]
    fn test_discover_tab_labels() {
        assert_eq!(DiscoverTab::default().label(), "Mods");
        assert_eq!(DiscoverTab::Modpacks.label(), "Modpacks");
        assert_eq!(DiscoverTab::Shaders.label(), "Shaders");
        assert_eq!(DiscoverTab::ResourcePacks.label(), "Resource Packs");
        assert_eq!(DiscoverTab::all().len(), 4);
    }
}
