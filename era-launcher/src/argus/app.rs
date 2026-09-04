//! Main app loop for ARGUS terminal UI.
//!
//! This module ties together the renderer, state, focus manager, and event
//! handler into the main event loop.

use crate::argus::Section;
use crate::argus::backend::{BackendBridge, LaunchEvent, RuntimeTracker};
use crate::argus::command::{CommandManager, CommandResult};
use crate::argus::focus::FocusManager;
use crate::argus::render::Renderer;
use crate::argus::state::{
    AppState, DiscoverPane, DiscoverTab, LogLevel, RuntimeState, SettingsEditMode,
};
use crate::argus::theme;
use crate::argus::ui;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::env;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Number of navbar targets registered before section content targets.
fn nav_count() -> usize {
    Section::all().len()
}

/// Lines scrolled per PgUp/PgDn/wheel notch in the LOGS view.
const LOG_SCROLL_STEP: usize = 5;

/// Shorten a label to `max` visible chars with an ellipsis.
fn truncate_label(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", head)
    }
}

/// The main ARGUS application
pub struct ArgusApp {
    state: AppState,
    focus: FocusManager,
    renderer: Renderer,
    tracker: RuntimeTracker,
    update_rx: Option<std::sync::mpsc::Receiver<crate::argus::update::UpdateCheckResult>>,
    update_quit_rx: Option<std::sync::mpsc::Receiver<()>>,
    mod_update_rx: Option<std::sync::mpsc::Receiver<Vec<crate::argus::state::UpdatableMod>>>,
    _last_resize_size: Option<(u16, u16)>,
}

impl ArgusApp {
    /// Create a new ARGUS application
    pub fn new() -> anyhow::Result<Self> {
        let renderer = Renderer::init()?;
        // Apply the persisted theme before the first frame so light/dark
        // settings are visible immediately.
        theme::apply(&BackendBridge::get_settings().theme);
        Ok(Self {
            state: AppState::new(),
            focus: FocusManager::new(),
            renderer,
            tracker: RuntimeTracker::new(),
            update_rx: None,
            update_quit_rx: None,
            mod_update_rx: None,
            _last_resize_size: None,
        })
    }

    /// Run the main event loop
    pub fn run(&mut self) -> anyhow::Result<()> {
        // Initial load
        self.refresh_data();
        self.setup_focus_targets();
        let current_version = self.state.current_version;
        // Clear any pending update-pending marker. The marker is written when
        // an update is staged; if the process is running again it means either
        // the .bat helper failed to apply the update, or it succeeded and the
        // relaunched binary is now running. In both cases we re-check (the
        // version check will correctly report up-to-date if the .bat succeeded).
        if let Ok(path) = std::env::var("ERA_LAUNCHER_UPDATE_PENDING") {
            let _ = std::fs::remove_file(&path);
        }
        let last_check = self.state.last_update_check;
        self.update_rx = Some(crate::argus::update::spawn_check(
            current_version,
            last_check,
        ));
        self.renderer.render(&self.state, &self.focus)?;

        let poll_timeout = Duration::from_millis(1000);
        let mut render_needed = true;

        std::panic::set_hook(Box::new(|info| {
            let crash_dir = crate::platform::Paths::new().data_local.join("crash");
            let _ = std::fs::create_dir_all(&crash_dir);
            let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let path = crash_dir.join(format!("era-launcher-panic-{}.log", ts));
            let _ = std::fs::write(
                &path,
                format!("ARGUS panicked at {}\npanic info: {}\n", ts, info),
            );
        }));

        loop {
            // Periodically poll runtime state and process output
            BackendBridge::poll_process_output(&mut self.state, &mut self.tracker);
            // Drain progress events from an in-flight background launch
            self.poll_launch_events();
            // Pick up the finished update check (single message, then stop)
            if let Some(rx) = &self.update_rx {
                if let Ok(result) = rx.try_recv() {
                    self.state.last_update_check = Some(std::time::Instant::now());
                    match result {
                        crate::argus::update::UpdateCheckResult::UpdateAvailable(tag) => {
                            self.state.update_check =
                                crate::argus::update::UpdateCheckResult::UpdateAvailable(
                                    tag.clone(),
                                );
                            self.state.log(
                                LogLevel::Info,
                                "ARGUS",
                                &format!(
                                    "Update available: {} (running v{}) — downloading...",
                                    tag,
                                    env!("CARGO_PKG_VERSION")
                                ),
                            );
                            self.state
                                .set_loading(true, Some(format!("Downloading update v{}...", tag)));
                            self.state.log(
                                LogLevel::Info,
                                "ARGUS",
                                &format!(
                                    "Update available: {} -- installing and restarting",
                                    tag
                                ),
                            );
                            self.renderer.render(&self.state, &self.focus)?;
                            let current_exe = match std::env::current_exe() {
                                Ok(p) => p,
                                Err(e) => {
                                    self.state.set_loading(false, None);
                                    self.state
                                        .set_error(format!("Cannot locate launcher path: {}", e));
                                    continue;
                                }
                            };
                            // Run the download + helper-spawn synchronously so we know
                            // it's staged before we exit. The .bat helper will copy the
                            // new exe over us after we exit, then relaunch.
                            let dest = current_exe
                                .parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or_else(|| std::path::PathBuf::from("."))
                                .join("era-launcher.new");
                            let result: Result<(), String> = (|| -> Result<(), String> {
                                let url = crate::argus::update::fetch_latest_asset_url()?;
                                crate::argus::update::download_asset(&url, &dest)?;
                                crate::argus::update::create_update_helper(&current_exe, &dest)?;
                                Ok(())
                            })();
                            if let Err(e) = result {
                                let _ = std::fs::remove_file(&dest);
                                eprintln!("Update failed: {}", e);
                                self.state.set_loading(false, None);
                                self.state.set_error(format!("Update failed: {}", e));
                            } else {
                                // Spawn the helper in a fully detached process so
                                // it survives our exit, then exit immediately. The
                                // helper waits for us to disappear, copies the new
                                // exe over us, and relaunches.
                                #[cfg(windows)]
                                let helper_path = current_exe.with_extension("bat");
                                #[cfg(not(windows))]
                                let helper_path = current_exe.with_extension("update.sh");
                                #[cfg(not(windows))]
                                let mut helper = std::process::Command::new("setsid");
                                #[cfg(not(windows))]
                                {
                                    helper
                                    .arg("-f")
                                    .arg("sh")
                                    .arg("-c")
                                    .arg(format!(
                                        "nohup '{}' >/dev/null 2>&1 &",
                                        helper_path.to_string_lossy()
                                    ));
                                }
                                #[cfg(windows)]
                                let mut helper = {
                                    let mut cmd = std::process::Command::new(&helper_path);
                                    // DETACHED_PROCESS = 0x00000008
                                    cmd.creation_flags(0x00000008);
                                    cmd
                                };
                                let _ = helper.spawn();
                                self.state.should_quit = true;
                            }
                        }
                        crate::argus::update::UpdateCheckResult::CheckFailed(err) => {
                            self.state.update_check =
                                crate::argus::update::UpdateCheckResult::CheckFailed(err.clone());
                            self.state.log(
                                LogLevel::Warn,
                                "ARGUS",
                                &format!("Update check failed: {}", err),
                            );
                        }
                        crate::argus::update::UpdateCheckResult::UpToDate => {
                            self.state.update_check =
                                crate::argus::update::UpdateCheckResult::UpToDate;
                        }
                    }
                    self.update_rx = None;
                }
            }

            if let Some(rx) = &self.update_quit_rx {
                if let Ok(()) = rx.try_recv() {
                    self.state.set_loading(false, None);
                    self.state.should_quit = true;
                }
            }

            if let Some(rx) = &self.mod_update_rx {
                if let Ok(updates) = rx.try_recv() {
                    self.state.updatable_mods = updates;
                    self.mod_update_rx = None;
                    self.state.log(
                        LogLevel::Info,
                        "ARGUS",
                        &format!(
                            "{} mod(s) with updates available",
                            self.state.updatable_mods.len()
                        ),
                    );
                }
            }

            // Once Minecraft is actually running, hide ARGUS and hand the
            // terminal to the game; restore ARGUS when the game exits.
            if self.state.runtime_state == RuntimeState::Running && self.tracker.has_process() {
                self.enter_game_mode()?;
                continue;
            }

            // Drive the loading spinner animation — only when loading
            let needs_spinner = self.state.loading;
            if needs_spinner {
                self.state.tick = self.state.tick.wrapping_add(1);
            }

            // Only render when state has changed or animation needs updating
            if render_needed || needs_spinner {
                self.renderer.render(&self.state, &self.focus)?;
                render_needed = false;
            }
            // Read input events with adaptive timeout — longer when idle
            let effective_timeout = if needs_spinner {
                Duration::from_millis(50)
            } else {
                poll_timeout
            };
            if let Some(event) = self.renderer.read_event(effective_timeout) {
                match event {
                    Event::Key(key) => {
                        // Only process key press events (not release or repeat)
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        if self.state.search_mode {
                            self.handle_search_input(key);
                        } else if self.state.loader_selector_open {
                            self.handle_loader_input(key);
                        } else if self.state.version_selector_open {
                            self.handle_version_input(key);
                        } else if self.state.pending_install.is_some() {
                            self.handle_install_version_input(key);
                        } else if self.state.account_selector_open || self.state.account_input_mode
                        {
                            self.handle_account_input(key);
                        } else if self.state.settings_edit_mode == SettingsEditMode::CustomJvmEditor
                        {
                            self.handle_custom_jvm_input(key);
                        } else if self.state.command_prompt_active {
                            self.handle_command_input(key);
                        } else {
                            self.handle_key(key);
                        }
                    }
                    Event::Mouse(mouse) => {
                        self.handle_mouse(&mouse);
                    }
                    Event::Resize(cols, rows) => {
                        self.handle_resize(cols, rows);
                    }
                    _ => {}
                }
                render_needed = true;
            }
            // When idle (no events), sleep briefly to reduce CPU usage
            if !render_needed && !needs_spinner {
                std::thread::sleep(Duration::from_millis(50));
            }
            if self.state.should_quit {
                break;
            }
        }
        self.renderer.deinit()?;
        Ok(())
    }

    /// Refresh data from the backend. Heavy probes (Java detection, system
    /// scan) only run once — repeated process spawning on every keypress made
    /// the UI feel unresponsive.
    pub fn refresh_data(&mut self) {
        self.state
            .set_loading(true, Some("Loading instances...".to_string()));

        // Load instances from REAL backend
        self.state.instances = BackendBridge::list_instances();

        // Keep a valid selection
        if let Some(sel) = self.state.selected_instance.clone() {
            if !self.state.instances.iter().any(|i| i.id == sel.id) {
                self.state.selected_instance = None;
            }
        }
        if self.state.selected_instance.is_none() && !self.state.instances.is_empty() {
            let first = self.state.instances[0].clone();
            self.state.selected_instance = Some(first);
        }

        // Load Java installations from REAL JavaManager (cached)
        if self.state.java_installations.is_empty() {
            self.state.java_installations = BackendBridge::detect_java();
        }

        // Load versions from REAL backend
        if self.state.versions.is_empty() {
            self.state.versions = BackendBridge::get_minecraft_versions();
            self.state.fabric_versions = BackendBridge::get_fabric_loader_versions();
            self.state.forge_versions = BackendBridge::get_forge_versions();
        }

        // Load system scan results (cached)
        if self.state.scan_results.is_empty() {
            self.state.scan_results = BackendBridge::scan_system();
        }

        // Installed content + worlds for the selected instance
        let selected_id = self.state.selected_instance.as_ref().map(|i| i.id.clone());
        self.state.installed_content =
            BackendBridge::list_installed_content(selected_id.as_deref());
        self.state.worlds = BackendBridge::list_worlds(selected_id.as_deref());

        // Crash reports + diagnostics
        self.state.crash_reports = BackendBridge::scan_crash_reports(selected_id.as_deref());
        if let Some(first) = self.state.crash_reports.first() {
            let text = std::fs::read_to_string(&first.path).unwrap_or_default();
            self.state.crash_diagnostics = crate::crashes::diagnose(&text);
        } else {
            self.state.crash_diagnostics.clear();
        }

        // Servers + screenshots
        ui::refresh_servers(&mut self.state);
        ui::refresh_screenshots(&mut self.state);

        // Accounts for the SETTINGS picker and header display
        let active_id = BackendBridge::active_account().map(|a| a.id);
        self.state.accounts = BackendBridge::list_accounts()
            .into_iter()
            .map(|a| (a.id, a.name))
            .collect();
        self.state.active_account_name = active_id.and_then(|id| {
            self.state
                .accounts
                .iter()
                .find(|(aid, _)| *aid == id)
                .map(|(_, n)| n.clone())
        });

        // Update runtime state from tracker
        if self.state.runtime_state == RuntimeState::Running && !self.tracker.is_running() {
            self.state.runtime_state = RuntimeState::Stopped;
            self.state
                .log(LogLevel::Info, "BACKEND", "Minecraft process exited");
        }

        // Poll process output
        BackendBridge::poll_process_output(&mut self.state, &mut self.tracker);

        // Update instances_dir
        self.state.instances_dir =
            Some(BackendBridge::instances_dir().to_string_lossy().to_string());

        self.state.set_loading(false, None);
    }

    /// Set up focus targets based on current state
    fn setup_focus_targets(&mut self) {
        self.focus.clear();

        // Navigation targets (always first in focus list)
        for section in Section::all() {
            let nav_id = format!("nav_{}", section.label().to_lowercase().replace(' ', "_"));
            self.focus.register(&nav_id, section.label());
        }

        // Section-specific focus targets
        self.register_section_focus_targets();

        // Focus the first content item if available, otherwise stay on nav
        if self.focus.len() > nav_count() {
            self.focus.set(nav_count());
        } else {
            self.focus.set(0);
        }
    }

    /// Rebuild focus targets while keeping focus on the same target id when
    /// possible (used after data-driven target lists change).
    fn rebuild_targets_preserving_focus(&mut self) {
        let current_id = self.focus.current().map(|t| t.id.clone());
        self.setup_focus_targets();
        if let Some(id) = current_id {
            if !self.focus.set_by_id(&id) {
                self.focus.set(nav_count());
            }
        }
    }

    /// Register focus targets for the current section's content.
    fn register_section_focus_targets(&mut self) {
        use crate::argus::state::DiscoverTab;
        match self.state.current_section {
            Section::Home => {
                self.focus.register("home_create", "Create Instance");
                self.focus.register("home_open", "Open Folder");
                self.focus.register("home_play", "Play");
            }
            Section::Instances => {
                if self.state.instances.is_empty() {
                    // Every section MUST register at least one content target,
                    // otherwise focus falls back to the navbar (index 0) and
                    // the wrong tab highlights.
                    self.focus.register("instances_empty", "No instances yet");
                } else {
                    for (i, _) in self.state.instances.iter().enumerate() {
                        self.focus
                            .register(&format!("instance_{}", i), &format!("Instance {}", i + 1));
                    }
                    self.focus.register("instance_delete", "Delete Selected");
                }
                // Stop control while Minecraft runs
                if self.state.runtime_state == RuntimeState::Running {
                    self.focus.register("instance_stop", "Stop");
                }
            }
            Section::Discover => {
                // Category selector lives INSIDE the Discover view
                let tabs = [
                    DiscoverTab::Mods,
                    DiscoverTab::Modpacks,
                    DiscoverTab::Shaders,
                    DiscoverTab::ResourcePacks,
                ];
                for (i, tab) in tabs.iter().enumerate() {
                    let label = match tab {
                        DiscoverTab::Mods => "Category: Mods",
                        DiscoverTab::Modpacks => "Category: Modpacks",
                        DiscoverTab::Shaders => "Category: Shaders",
                        DiscoverTab::ResourcePacks => "Category: Resource Packs",
                    };
                    self.focus.register(&format!("disc_cat_{}", i), label);
                }
                self.focus.register("disc_search", "Search Modrinth");
                // Only VISIBLE results get focus targets (installed ones are
                // hidden unless the `i` reveal toggle is on). Ids carry the
                // REAL index so activation maps straight into results.
                for i in self.state.visible_project_indices() {
                    self.focus
                        .register(&format!("project_{}", i), &format!("Project {}", i + 1));
                }
            }
            Section::Mods => {
                if self.state.installed_content.is_empty() {
                    self.focus
                        .register("mods_placeholder", "No content installed");
                } else {
                    for (i, _) in self.state.installed_content.iter().enumerate() {
                        self.focus.register(
                            &format!("installed_{}", i),
                            &format!("Installed item {}", i + 1),
                        );
                    }
                }
                for (i, _) in self.state.updatable_mods.iter().enumerate() {
                    self.focus
                        .register(&format!("update_{}", i), &format!("Update {}", i + 1));
                }
            }
            Section::Worlds => {
                if self.state.worlds.is_empty() {
                    self.focus.register("worlds_list", "Worlds List");
                } else {
                    for (i, _) in self.state.worlds.iter().enumerate() {
                        self.focus
                            .register(&format!("world_{}", i), &format!("World {}", i + 1));
                    }
                }
            }
            Section::Servers => {
                if self.state.servers.is_empty() {
                    self.focus.register("servers_empty", "No servers yet");
                } else {
                    for (i, _) in self.state.servers.iter().enumerate() {
                        self.focus.register(
                            &format!("server_{}", i),
                            &format!("Server {}", i + 1),
                        );
                    }
                }
                self.focus.register("server_add", "Add server");
            }
            Section::Logs => {
                if self.state.logs.is_empty() {
                    self.focus.register("logs_empty", "No log entries");
                } else {
                    self.focus.register("logs_list", "Log Entries");
                }
                if self.state.live_log_view {
                    self.focus.register("logs_live", "Live log");
                }
            }
            Section::Crashes => {
                if self.state.crash_reports.is_empty() {
                    self.focus.register("crashes_empty", "No crash reports");
                } else {
                    self.focus.register("crashes_list", "Crash Reports");
                    self.focus.register("crashes_copy", "Copy report");
                    self.focus.register("crashes_delete", "Delete report");
                    self.focus.register("crashes_open", "Open folder");
                }
            }
            Section::Screenshots => {
                if self.state.screenshots.is_empty() {
                    self.focus.register("screenshots_empty", "No screenshots");
                } else {
                    for (i, _) in self.state.screenshots.iter().enumerate() {
                        self.focus.register(
                            &format!("screenshot_{}", i),
                            &format!("Screenshot {}", i + 1),
                        );
                    }
                    self.focus.register("screenshots_open", "Open folder");
                }
            }
            Section::Settings => {
                self.focus.register("settings_memory", "Default Memory");
                self.focus.register("settings_java", "Java Path");
                self.focus.register("settings_theme", "Theme");
                self.focus.register("settings_language", "Language");
                self.focus.register("settings_optimization", "Optimization");
                self.focus.register("settings_account", "Offline Account");
                self.focus.register("settings_window", "Window");
                self.focus
                    .register("settings_java_list", "Java Installations");
                // Register Java installation focus targets
                for (i, _j) in self.state.java_installations.iter().enumerate() {
                    self.focus
                        .register(&format!("java_{}", i), &format!("Java {}", i + 1));
                }
            }
        }
    }

    /// Handle keyboard input (when command prompt and search mode inactive)
    fn handle_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // Help overlay swallows everything except its close keys.
        if self.state.help_overlay {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => self.state.help_overlay = false,
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') if !ctrl && !alt => {
                self.exit();
            }
            KeyCode::Char('c') | KeyCode::Char('C') if ctrl => {
                self.exit();
            }
            // Arrow keys: Left/Right switch between top-level sections
            KeyCode::Left => {
                self.previous_section();
            }
            KeyCode::Right => {
                self.next_section();
            }
            // Enter activates the focused item
            KeyCode::Enter => {
                if self.state.settings_edit_mode.should_use_edit_keys() {
                    self.apply_settings_edit();
                } else if let Some(current) = self.focus.current() {
                    let target_id = current.id.clone();
                    if target_id.starts_with("nav_") {
                        self.enter_current_section();
                    } else {
                        self.handle_activate(&target_id);
                    }
                }
            }
            // Up/Down navigate items WITHIN the current section's content —
            // they wrap inside the list and never jump back to the navbar.
            // In DISCOVER they are scoped to the active pane.
            KeyCode::Up => {
                if self.state.settings_edit_mode.should_use_edit_keys() {
                    self.settings_edit_up();
                } else if self.state.current_section == Section::Discover {
                    self.discover_step(-1);
                } else {
                    self.content_step(-1);
                }
            }
            KeyCode::Down => {
                if self.state.settings_edit_mode.should_use_edit_keys() {
                    self.settings_edit_down();
                } else if self.state.current_section == Section::Discover {
                    self.discover_step(1);
                } else {
                    self.content_step(1);
                }
            }
            // Page keys: fast scrolling — LOGS scrolls history, DISCOVER
            // steps ±5 inside its active pane, other sections jump focus.
            KeyCode::PageUp => {
                if self.state.current_section == Section::Logs {
                    self.scroll_logs(-(LOG_SCROLL_STEP as isize));
                } else if self.state.current_section == Section::Discover {
                    self.discover_jump(-5);
                } else {
                    self.jump_focus(-5);
                }
            }
            KeyCode::PageDown => {
                if self.state.current_section == Section::Logs {
                    self.scroll_logs(LOG_SCROLL_STEP as isize);
                } else if self.state.current_section == Section::Discover {
                    self.discover_jump(5);
                } else {
                    self.jump_focus(5);
                }
            }
            KeyCode::Home => {
                if self.state.current_section == Section::Logs {
                    self.state.log_scroll = 0;
                } else {
                    let last = self.focus.len().saturating_sub(1);
                    self.focus.set(nav_count().min(last));
                }
            }
            KeyCode::End => {
                if self.state.current_section == Section::Logs {
                    let max = self.state.logs.len().saturating_sub(2);
                    self.state.log_scroll = max;
                } else {
                    let last = self.focus.len().saturating_sub(1);
                    self.focus.set(last);
                }
            }
            // TAB / SHIFT+TAB move focus exactly one step
            KeyCode::Tab => {
                self.focus.next();
            }
            KeyCode::BackTab => {
                self.focus.previous();
            }
            // Command prompt
            KeyCode::Char('l') if ctrl => {
                self.open_command_prompt();
            }
            // Live log tail toggle (Shift+L arrives as uppercase 'L' when shift
            // is held, but crossterm still delivers Char('l') — bind under a
            // different letter to avoid the conflict with the Logs section
            // shortcut and Ctrl+L command prompt opener).
            KeyCode::Char('L')
                if shift && !ctrl && !alt && self.state.current_section == Section::Logs =>
            {
                self.handle_activate("logs_live");
            }
            // Create Instance shortcut (when on HOME section)
            KeyCode::Char('c') if !ctrl && !alt && self.state.current_section == Section::Home => {
                self.handle_activate("home_create");
            }
            // Search shortcut inside DISCOVER
            KeyCode::Char('/') | KeyCode::Char('f')
                if !ctrl && !alt && self.state.current_section == Section::Discover =>
            {
                self.activate_search();
            }
            // Help overlay toggle
            KeyCode::Char('?') if !ctrl && !alt => {
                self.state.help_overlay = true;
            }
            // Number keys switch Discover categories instantly
            KeyCode::Char(c @ '1'..='4')
                if !ctrl && !alt && self.state.current_section == Section::Discover =>
            {
                let idx = (c as u8 - b'1') as usize;
                self.switch_discover_category(idx);
            }
            // Reveal/hide already-installed projects in DISCOVER
            KeyCode::Char('i') | KeyCode::Char('I')
                if !ctrl && !alt && self.state.current_section == Section::Discover =>
            {
                self.state.show_installed_discover = !self.state.show_installed_discover;
                let showing = self.state.show_installed_discover;
                self.rebuild_targets_preserving_focus();
                let n = self.state.discover_hidden_count;
                self.state.set_status(
                    if showing {
                        format!("Showing installed projects too ({n} marked ✓)")
                    } else {
                        format!("Installed projects hidden again ({n})")
                    }
                );
            }
            // Delete selected instance
            KeyCode::Char('x')
                if !ctrl && !alt && self.state.current_section == Section::Instances =>
            {
                self.delete_selected_instance();
            }
            // Remove the focused installed file (mods/shaders/resourcepacks)
            KeyCode::Char('x') if !ctrl && !alt && self.state.current_section == Section::Mods => {
                self.remove_focused_content();
            }
            // Worlds section shortcuts
            KeyCode::Char('b')
                if !ctrl && !alt && self.state.current_section == Section::Worlds =>
            {
                self.backup_focused_world();
            }
            KeyCode::Char('o')
                if !ctrl && !alt && self.state.current_section == Section::Worlds =>
            {
                self.open_focused_world_folder();
            }
            KeyCode::Char('O') | KeyCode::Char('R')
                if shift && !ctrl && !alt && self.state.current_section == Section::Worlds =>
            {
                self.open_worlds_save_folder();
            }
            KeyCode::Char('d')
                if !ctrl && !alt && self.state.current_section == Section::Worlds =>
            {
                self.delete_focused_world();
            }
            // Add server
            KeyCode::Char('a')
                if !ctrl && !alt && self.state.current_section == Section::Servers =>
            {
                self.handle_activate("server_add");
            }
            // Section shortcuts
            KeyCode::Char(c) if !ctrl && !alt => {
                self.handle_section_shortcut(c);
            }
            KeyCode::Esc => {
                if self.state.search_mode {
                    self.state.search_mode = false;
                } else if self.state.current_section == Section::Discover {
                    // DISCOVER ESC stack: results → categories → main tabs.
                    if self.state.discover_pane == DiscoverPane::Results {
                        self.state.discover_pane = DiscoverPane::Categories;
                        self.focus.set_by_id(&self.active_category_id());
                        self.state
                            .set_status("Back to categories — ESC again for main tabs".to_string());
                    } else {
                        self.focus.set_by_id("nav_discover");
                        self.state
                            .set_status("Main tabs — ←→ to switch, ENTER to re-enter".to_string());
                    }
                } else if self.state.settings_edit_mode.should_use_edit_keys() {
                    self.cancel_settings_edit();
                } else {
                    self.state.clear_error();
                }
            }
            _ => {}
        }
    }

    /// Move focus by `delta` steps WITHIN the section's content targets,
    /// wrapping at the ends. Never lands on the navbar — reaching the tabs
    /// is TAB's job. From a navbar position, Down enters the first item and
    /// Up enters the last.
    fn content_step(&mut self, delta: isize) {
        let total = self.focus.len();
        let n = nav_count();
        if total <= n {
            return;
        }
        let count = (total - n) as isize;
        let cur = self.focus.current_index() as isize - n as isize;
        let cur_in_content = if cur < 0 {
            if delta > 0 { -1 } else { 0 }
        } else {
            cur
        };
        let next = ((cur_in_content + delta) % count + count) % count;
        self.focus.set(n + next as usize);
    }

    /// Move focus by `delta` steps, clamped to the section's CONTENT targets
    /// (never lands on the navbar).
    fn jump_focus(&mut self, delta: isize) {
        let total = self.focus.len();
        if total <= nav_count() {
            return;
        }
        let cur = self.focus.current_index() as isize;
        let target = (cur + delta).clamp(nav_count() as isize, total as isize - 1);
        self.focus.set(target as usize);
    }

    /// Scroll the LOGS list by lines (positive = back in time).
    fn scroll_logs(&mut self, delta: isize) {
        let max = self.state.logs.len().saturating_sub(2) as isize;
        let next = (self.state.log_scroll as isize + delta).clamp(0, max.max(0));
        self.state.log_scroll = next as usize;
    }

    /// Switch the active DISCOVER category by index and reload results.
    /// Entering a category moves navigation INTO the results pane.
    fn switch_discover_category(&mut self, idx: usize) {
        let tabs = DiscoverTab::all();
        let Some(tab) = tabs.get(idx) else {
            return;
        };
        let changed = self.state.discover_tab != *tab;
        if changed {
            self.state.discover_tab = *tab;
            self.state.modrinth_results.clear();
        }
        // Re-entering the same category with results loaded: just jump into
        // the list, no refetch.
        if !changed && !self.state.modrinth_results.is_empty() {
            self.enter_results_pane();
            return;
        }
        self.fetch_discover_results("");
        self.enter_results_pane();
    }

    /// Move navigation into the results list (or bounce back to categories
    /// when there is nothing to show).
    fn enter_results_pane(&mut self) {
        let visible = self.state.visible_project_indices();
        if visible.is_empty() {
            self.state.discover_pane = DiscoverPane::Categories;
            self.focus.set_by_id(&self.active_category_id());
            return;
        }
        self.state.discover_pane = DiscoverPane::Results;
        self.focus.set_by_id(&format!("project_{}", visible[0]));
    }

    /// Focus id of the row matching the active category tab.
    fn active_category_id(&self) -> String {
        let idx = DiscoverTab::all()
            .iter()
            .position(|t| *t == self.state.discover_tab)
            .unwrap_or(0);
        format!("disc_cat_{}", idx)
    }

    /// ↑/↓ inside DISCOVER: scoped to whichever pane owns navigation.
    /// Categories pane cycles [4 category rows + search]; results pane
    /// cycles ONLY the result rows — never bleeds between panes or tabs.
    fn discover_step(&mut self, delta: isize) {
        match self.state.discover_pane {
            DiscoverPane::Categories => {
                let mut ids: Vec<String> = (0..DiscoverTab::all().len())
                    .map(|i| format!("disc_cat_{}", i))
                    .collect();
                ids.push("disc_search".to_string());
                self.step_within_ids(&ids, delta);
            }
            DiscoverPane::Results => {
                let ids: Vec<String> = self
                    .state
                    .visible_project_indices()
                    .into_iter()
                    .map(|i| format!("project_{}", i))
                    .collect();
                if ids.is_empty() {
                    self.state
                        .set_status("No results — pick another category (ESC)".to_string());
                    return;
                }
                self.step_within_ids(&ids, delta);
            }
        }
    }

    /// Wrap `delta` steps within an explicit ordered id group.
    fn step_within_ids(&mut self, ids: &[String], delta: isize) {
        if ids.is_empty() {
            return;
        }
        let cur_idx = ids
            .iter()
            .position(|id| self.focus.current().map(|f| f.id == *id).unwrap_or(false));
        let next = match cur_idx {
            Some(i) => {
                let count = ids.len() as isize;
                ((i as isize + delta) % count + count) % count
            }
            None => {
                if delta >= 0 {
                    0isize
                } else {
                    ids.len() as isize - 1
                }
            }
        };
        self.focus.set_by_id(&ids[next as usize]);
    }

    /// PageUp/PageDown inside DISCOVER — same pane scoping, ±5 steps.
    fn discover_jump(&mut self, delta: isize) {
        for _ in 0..5 {
            self.discover_step(delta.signum());
        }
    }

    // ===== Offline account picker =====

    /// Handle keys while the account selector or the new-name input is open.
    fn handle_account_input(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if self.state.account_input_mode {
            match key.code {
                KeyCode::Enter => {
                    let name = self.state.account_input.trim().to_string();
                    match BackendBridge::create_offline_account(&name) {
                        Ok(acc) => {
                            let _ = BackendBridge::set_active_account(&acc.id);
                            self.state.account_input_mode = false;
                            self.state.account_selector_open = false;
                            self.state.set_status(format!(
                                "Offline account '{}' created and selected",
                                acc.name
                            ));
                            self.state.log(
                                LogLevel::Info,
                                "BACKEND",
                                &format!(
                                    "Offline account '{}' created (uuid {}) — persisted",
                                    acc.name, acc.uuid
                                ),
                            );
                            self.refresh_data();
                            self.rebuild_targets_preserving_focus();
                        }
                        Err(e) => {
                            // Stay in input mode so the user can fix the name.
                            self.state.set_error(e);
                        }
                    }
                }
                KeyCode::Esc => {
                    self.state.account_input_mode = false;
                    self.state.account_input.clear();
                }
                KeyCode::Backspace if !ctrl => {
                    self.state.account_input.pop();
                }
                KeyCode::Char(c) if !ctrl && self.state.account_input.len() < 16 => {
                    self.state.account_input.push(c);
                }
                _ => {}
            }
            return;
        }

        let row_count = self.state.accounts.len() + 1; // + "create new" row
        match key.code {
            KeyCode::Esc => {
                self.state.account_selector_open = false;
            }
            KeyCode::Up => {
                if self.state.account_selector_index > 0 {
                    self.state.account_selector_index -= 1;
                }
            }
            KeyCode::Down => {
                let max = row_count.saturating_sub(1);
                if self.state.account_selector_index < max {
                    self.state.account_selector_index += 1;
                }
            }
            KeyCode::Char('x') | KeyCode::Char('X') => {
                // Delete the highlighted account (not the create row).
                let idx = self.state.account_selector_index;
                if idx < self.state.accounts.len() {
                    let (id, name) = self.state.accounts[idx].clone();
                    match BackendBridge::delete_account(&id) {
                        Ok(()) => {
                            self.state.set_status(format!("Deleted account '{}'", name));
                            self.refresh_data();
                            let max = self.state.accounts.len(); // create row index
                            if self.state.account_selector_index > max {
                                self.state.account_selector_index = max;
                            }
                        }
                        Err(e) => self.state.set_error(e),
                    }
                }
            }
            KeyCode::Enter => {
                let idx = self.state.account_selector_index;
                if idx >= self.state.accounts.len() {
                    // "Create new" row → switch to text input.
                    self.state.account_input_mode = true;
                    self.state.account_input.clear();
                    return;
                }
                if let Some((id, name)) = self.state.accounts.get(idx).cloned() {
                    match BackendBridge::set_active_account(&id) {
                        Ok(()) => {
                            self.state.account_selector_open = false;
                            self.state
                                .set_status(format!("Launch account set to '{}'", name));
                            self.refresh_data();
                        }
                        Err(e) => self.state.set_error(e),
                    }
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.state.account_input_mode = true;
                self.state.account_input.clear();
            }
            _ => {}
        }
    }

    /// Open the offline-account picker from SETTINGS.
    fn open_account_selector(&mut self) {
        self.state.accounts = BackendBridge::list_accounts()
            .into_iter()
            .map(|a| (a.id, a.name))
            .collect();
        // Start on the active account's row when possible.
        let active_idx = BackendBridge::active_account()
            .and_then(|acc| self.state.accounts.iter().position(|(id, _)| *id == acc.id))
            .unwrap_or(self.state.accounts.len()); // or the create row
        self.state.account_selector_index = active_idx.min(self.state.accounts.len());
        self.state.account_selector_open = true;
    }

    // ===== Create-instance loader picker =====

    /// Handle keys while the loader picker overlay is open.
    fn handle_loader_input(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.state.loader_selector_open = false;
                self.state
                    .set_status("Instance creation cancelled".to_string());
            }
            KeyCode::Up => {
                if self.state.loader_selector_index > 0 {
                    self.state.loader_selector_index -= 1;
                }
            }
            KeyCode::Down => {
                let max = crate::argus::state::CREATE_LOADERS.len().saturating_sub(1);
                if self.state.loader_selector_index < max {
                    self.state.loader_selector_index += 1;
                }
            }
            KeyCode::Enter => {
                let idx = self.state.loader_selector_index;
                self.open_version_picker(idx);
            }
            KeyCode::Char(c) if !ctrl => {
                // Quick-select: press the number of a row.
                if let Some(d) = c.to_digit(10) {
                    let idx = (d as usize).wrapping_sub(1);
                    if idx < crate::argus::state::CREATE_LOADERS.len() {
                        self.open_version_picker(idx);
                    }
                }
            }
            _ => {}
        }
    }

    /// Create-flow step 2: loader chosen → open the game-version picker
    /// (live Mojang release list, type-to-filter).
    fn open_version_picker(&mut self, idx: usize) {
        let Some((loader_id, _)) = crate::argus::state::CREATE_LOADERS.get(idx) else {
            return;
        };
        let loader = loader_id.to_string();
        if loader == "forge" {
            self.state.loader_selector_open = false;
            self.state.set_error(
                "Forge is not supported by the terminal launcher yet — pick vanilla, fabric or quilt"
                    .to_string(),
            );
            return;
        }
        self.state.pending_version_loader = Some(loader);
        // Load the real release list once (network); static fallback offline.
        if self.state.versions.len() < 10 {
            self.state
                .set_loading(true, Some("Fetching Minecraft versions...".to_string()));
            let _ = self.renderer.render(&self.state, &self.focus);
            self.state.versions = BackendBridge::fetch_minecraft_releases();
            self.state.set_loading(false, None);
        }
        // MUST close the loader overlay — the input router checks
        // loader_selector_open first, and with both open every keypress was
        // swallowed by the hidden loader handler (version list looked dead).
        self.state.loader_selector_open = false;
        self.state.version_filter.clear();
        self.state.version_selector_index = 0;
        self.state.version_selector_open = true;
    }

    /// Filter the version list by the picker's live query (case-insensitive
    /// substring).
    fn filtered_game_versions(&self) -> Vec<String> {
        let needle = self.state.version_filter.to_lowercase();
        self.state
            .versions
            .iter()
            .filter(|v| needle.is_empty() || v.to_lowercase().contains(&needle))
            .cloned()
            .collect()
    }

    /// Handle keys while the game-version picker is open.
    fn handle_version_input(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let list = self.filtered_game_versions();
        match key.code {
            KeyCode::Esc => {
                // Back to the loader step (not full cancel).
                self.state.version_selector_open = false;
                self.state.pending_version_loader = None;
                self.state.version_filter.clear();
                self.state.loader_selector_open = true;
            }
            KeyCode::Up => {
                if self.state.version_selector_index > 0 {
                    self.state.version_selector_index -= 1;
                }
            }
            KeyCode::Down => {
                if self.state.version_selector_index + 1 < list.len() {
                    self.state.version_selector_index += 1;
                }
            }
            KeyCode::PageUp => {
                self.state.version_selector_index =
                    self.state.version_selector_index.saturating_sub(10);
            }
            KeyCode::PageDown => {
                if !list.is_empty() {
                    self.state.version_selector_index =
                        (self.state.version_selector_index + 10).min(list.len() - 1);
                }
            }
            KeyCode::Backspace if !ctrl => {
                self.state.version_filter.pop();
                self.state.version_selector_index = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                if !c.is_control() && self.state.version_filter.len() < 16 {
                    self.state.version_filter.push(c);
                    self.state.version_selector_index = 0;
                }
            }
            KeyCode::Enter => {
                let Some(version) = list.get(self.state.version_selector_index).cloned() else {
                    self.state
                        .set_status("No version matches the filter".to_string());
                    return;
                };
                let loader = self
                    .state
                    .pending_version_loader
                    .clone()
                    .unwrap_or_else(|| "vanilla".to_string());
                self.state.version_selector_open = false;
                self.state.pending_version_loader = None;
                self.state.version_filter.clear();
                match BackendBridge::create_instance_full(&mut self.state, &loader, &version) {
                    Ok(instance) => {
                        self.state.selected_instance = Some(instance.clone());
                        self.state.set_status(format!(
                            "Created {} instance '{}' on Minecraft {}",
                            instance.loader, instance.name, instance.game_version
                        ));
                        self.state.log(
                            LogLevel::Info,
                            "BACKEND",
                            &format!(
                                "Created instance '{}' — loader={} version={} persisted",
                                instance.name, instance.loader, instance.game_version
                            ),
                        );
                        self.refresh_data();
                        self.rebuild_targets_preserving_focus();
                    }
                    Err(e) => self.state.set_error(e),
                }
            }
            _ => {}
        }
    }

    /// Handle keystrokes while the DISCOVER search bar is capturing input
    fn handle_search_input(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => {
                let query = self.state.discover_search.trim().to_string();
                self.state.search_mode = false;
                self.fetch_discover_results(&query);
                // A successful search drops you into the results list.
                self.enter_results_pane();
            }
            KeyCode::Esc => {
                self.state.search_mode = false;
            }
            KeyCode::Backspace if !ctrl => {
                self.state.discover_search.pop();
            }
            KeyCode::Char(c) if !ctrl => {
                self.state.discover_search.push(c);
            }
            _ => {}
        }
    }

    /// Handle keyboard input in command prompt mode
    fn handle_command_input(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => {
                let input = self.state.command_input.clone();
                // Record history BEFORE executing so ↑/↓ can recall it.
                self.state.add_to_history(&input);
                self.state.command_prompt_active = false;
                self.execute_command(&input);
            }
            KeyCode::Esc => {
                self.state.command_input.clear();
                self.state.command_prompt_active = false;
                self.state.history_position = 0;
            }
            KeyCode::Backspace if !ctrl => {
                self.state.command_input.pop();
            }
            KeyCode::Up => {
                self.state.previous_command();
            }
            KeyCode::Down => {
                self.state.next_command();
            }
            KeyCode::Char(c) if !ctrl => {
                self.state.command_input.push(c);
            }
            _ => {}
        }
    }

    /// Handle keyboard input in custom JVM args editor
    fn handle_custom_jvm_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let input = self.state.custom_jvm_input.trim().to_string();
                let profile_saved = BackendBridge::set_optimization_profile(
                    crate::minecraft::optimization::OptimizationProfile::Custom,
                );
                let args_saved = BackendBridge::set_custom_jvm_args(&input);
                if profile_saved && args_saved {
                    self.state.log(
                        LogLevel::Info,
                        "BACKEND",
                        &format!(
                            "Custom JVM args saved ({} args)",
                            input.split_whitespace().count()
                        ),
                    );
                } else {
                    self.state
                        .log(LogLevel::Error, "BACKEND", "Failed to save custom JVM args");
                }
                self.state.settings_edit_mode = SettingsEditMode::None;
                self.state.custom_jvm_input.clear();
            }
            KeyCode::Esc => {
                self.state.custom_jvm_input.clear();
                self.state.settings_edit_mode = SettingsEditMode::None;
                self.state
                    .log(LogLevel::Info, "ARGUS", "Custom JVM edit cancelled");
            }
            KeyCode::Backspace => {
                self.state.custom_jvm_input.pop();
            }
            KeyCode::Char(c) => {
                self.state.custom_jvm_input.push(c);
            }
            _ => {}
        }
    }

    /// Handle mouse events: click focuses/activates hit-tested targets,
    /// wheel scrolls lists.
    fn handle_mouse(&mut self, mouse: &MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(_) => {
                if self.state.help_overlay {
                    self.state.help_overlay = false;
                    return;
                }
                let Some(id) = ui::hit_test(mouse.column, mouse.row) else {
                    return;
                };

                // Actionable controls activate on single click; list rows
                // only take focus (ENTER then activates, mirroring keyboard).
                let actionable = id.starts_with("nav_")
                    || id.starts_with("home_")
                    || id.starts_with("disc_cat_")
                    || id == "disc_search"
                    || id.starts_with("settings_")
                    || id == "instance_delete";

                if self.focus.set_by_id(&id) && actionable {
                    self.handle_activate(&id);
                    if self.state.search_mode {
                        // Clicking the search field starts capture mode too.
                    }
                }
                // Clicking a result row implies you're browsing results.
                if id.starts_with("project_") && self.state.current_section == Section::Discover {
                    self.state.discover_pane = DiscoverPane::Results;
                }
            }
            MouseEventKind::ScrollUp => {
                if self.state.help_overlay || self.state.command_prompt_active {
                    return;
                }
                if self.state.current_section == Section::Logs {
                    self.scroll_logs(-(LOG_SCROLL_STEP as isize));
                } else if self.state.current_section == Section::Discover {
                    self.discover_step(-1);
                } else {
                    self.content_step(-1);
                }
            }
            MouseEventKind::ScrollDown => {
                if self.state.help_overlay || self.state.command_prompt_active {
                    return;
                }
                if self.state.current_section == Section::Logs {
                    self.scroll_logs(LOG_SCROLL_STEP as isize);
                } else if self.state.current_section == Section::Discover {
                    self.discover_step(1);
                } else {
                    self.content_step(1);
                }
            }
            _ => {}
        }
    }

    /// Handle terminal resize
    fn handle_resize(&mut self, cols: u16, rows: u16) {
        // Skip resize events that we generated ourselves via set_buffer_size
        if Renderer::is_spurious_resize(cols, rows) {
            return;
        }
        let prev = self._last_resize_size;
        self.renderer.on_resize(cols, rows);
        if Some((cols, rows)) != prev {
            self.state.log(
                crate::argus::state::LogLevel::Debug,
                "ARGUS",
                "Terminal resized",
            );
        }
        self._last_resize_size = Some((cols, rows));
    }

    /// Navigate to the previous top-level section (navbar only)
    fn previous_section(&mut self) {
        let sections = Section::all();
        let current_idx = sections
            .iter()
            .position(|s| *s == self.state.current_section)
            .unwrap_or(0);
        let new_idx = if current_idx == 0 {
            sections.len() - 1
        } else {
            current_idx - 1
        };
        self.navigate_to_section(sections[new_idx]);
    }

    /// Navigate to the next top-level section (navbar only)
    fn next_section(&mut self) {
        let sections = Section::all();
        let current_idx = sections
            .iter()
            .position(|s| *s == self.state.current_section)
            .unwrap_or(0);
        let new_idx = (current_idx + 1) % sections.len();
        self.navigate_to_section(sections[new_idx]);
    }

    /// Remember the focused content target for a section so revisiting
    /// restores where you were.
    fn save_focus_memory(&mut self, section: Section) {
        if let Some(cur) = self.focus.current() {
            let id = cur.id.clone();
            if id.starts_with("nav_") {
                return;
            }
            if let Some(entry) = self
                .state
                .focus_memory
                .iter_mut()
                .find(|(s, _)| *s == section)
            {
                entry.1 = id;
            } else {
                self.state.focus_memory.push((section, id));
            }
        }
    }

    /// Look up a remembered focus target for a section.
    fn remembered_focus(&self, section: Section) -> Option<String> {
        self.state
            .focus_memory
            .iter()
            .find(|(s, _)| *s == section)
            .map(|(_, id)| id.clone())
    }

    /// Navigate to a section, refresh data, and put focus INSIDE the
    /// section's content (remembered position when revisiting). Previously
    /// this parked focus on the navbar entry, so the highlight appeared to
    /// "go back to the main tabs" after every switch.
    fn navigate_to_section(&mut self, section: Section) {
        self.save_focus_memory(self.state.current_section);
        self.state.navigate_to(section);
        self.on_section_entered();
    }

    /// Common work whenever the visible section changes: refresh data,
    /// rebuild focus targets, restore remembered content focus (or seed the
    /// first content item), lazy-load Discover data.
    /// If the selected instance changed since the current DISCOVER results
    /// were fetched (different id OR version), clear and re-fetch so the
    /// list reflects the NEW instance's version/loader facets and its own
    /// installed-hidden set — never leftovers from a previous instance.
    fn ensure_discover_fresh(&mut self) {
        if self.state.current_section != Section::Discover {
            return;
        }
        let sel = self.state.selected_instance.as_ref().map(|i| i.id.clone());
        let version_changed = sel.is_some()
            && self.state.discover_game_version
                != self
                    .state
                    .selected_instance
                    .as_ref()
                    .map(|i| i.game_version.clone())
                    .unwrap_or_default();
        if self.state.discover_scoped_instance != sel || version_changed {
            // Reset search context too: it belonged to the old instance.
            self.state.discover_search.clear();
            self.state.modrinth_results.clear();
            self.state.result_installed.clear();
            self.state.discover_hidden_count = 0;
            // Refresh FIRST so installed_content belongs to the new instance
            // when fetch prunes against it.
            self.refresh_data();
            self.fetch_discover_results("");
            // Entry UX stays on the category pane; fresh list waits behind it.
            self.state.discover_pane = DiscoverPane::Categories;
            self.focus.set_by_id(&self.active_category_id());
        }
    }

    fn on_section_entered(&mut self) {
        // Drop stale results when the selected instance changed since the
        // last fetch (new instance ⇒ new version facets + install set).
        self.ensure_discover_fresh();
        self.refresh_data();
        self.setup_focus_targets();

        if self.state.current_section == Section::Discover {
            // DISCOVER always lands on the CATEGORY pane — pick a category
            // first, then ESC-stacking takes you results → categories → tabs.
            self.state.discover_pane = DiscoverPane::Categories;
            self.focus.set_by_id(&self.active_category_id());
        } else if let Some(id) = self.remembered_focus(self.state.current_section) {
            // Other sections restore where you were; setup_focus_targets
            // already seeded the first content item as fallback.
            self.focus.set_by_id(&id);
        }

        if self.state.current_section == Section::Discover && self.state.modrinth_results.is_empty()
        {
            self.fetch_discover_results("");
            self.state.discover_pane = DiscoverPane::Categories;
            self.focus.set_by_id(&self.active_category_id());
        }

        if self.state.current_section == Section::Mods && self.mod_update_rx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.mod_update_rx = Some(rx);
            let instance_id = self.state.selected_instance.as_ref().map(|i| i.id.clone());
            std::thread::spawn(move || {
                let updates = BackendBridge::check_mod_updates(instance_id.as_deref());
                let _ = tx.send(updates);
            });
        }
    }

    /// Enter the current section: move focus from the navbar into the
    /// section's first content item (or its remembered position).
    fn enter_current_section(&mut self) {
        // Same staleness guard as on_section_entered (ENTER-on-tab path).
        self.ensure_discover_fresh();
        self.refresh_data();
        self.setup_focus_targets();
        if self.state.current_section == Section::Discover {
            // ENTER on the DISCOVER tab → category pane, per the ESC stack.
            self.state.discover_pane = DiscoverPane::Categories;
            self.focus.set_by_id(&self.active_category_id());
            if self.state.modrinth_results.is_empty() {
                self.fetch_discover_results("");
                self.state.discover_pane = DiscoverPane::Categories;
                self.focus.set_by_id(&self.active_category_id());
            }
        } else if let Some(mem) = self.remembered_focus(self.state.current_section) {
            self.focus.set_by_id(&mem);
        }
    }

    /// Navigate to a section by shortcut key
    fn handle_section_shortcut(&mut self, c: char) {
        let target_section = match c {
            'h' => Some(Section::Home),
            'd' => Some(Section::Discover),
            'i' => Some(Section::Instances),
            'm' => Some(Section::Mods),
            's' => Some(Section::Settings),
            'l' => Some(Section::Logs),
            'w' => Some(Section::Worlds),
            'v' => Some(Section::Servers),
            'p' => Some(Section::Screenshots),
            'c' => Some(Section::Crashes),
            _ => None,
        };

        if let Some(section) = target_section {
            self.navigate_to_section(section);
        }
    }

    /// Flag which DISCOVER results are already installed in the selected
    /// instance. They stay in the list (hidden unless the user toggles `i`)
    /// so their entry remains reachable for version switching.
    fn classify_installed_results(&mut self) {
        let sel_id = self.state.selected_instance.as_ref().map(|i| i.id.clone());
        let recorded: std::collections::HashSet<String> =
            BackendBridge::install_record_ids(sel_id.as_deref())
                .into_iter()
                .collect();
        let names: Vec<String> = self
            .state
            .installed_content
            .iter()
            .map(|c| c.name.clone())
            .collect();

        self.state.result_installed = self
            .state
            .modrinth_results
            .iter()
            .map(|p| {
                if recorded.contains(&p.id) {
                    return true;
                }
                names
                    .iter()
                    .any(|n| BackendBridge::title_matches_file(&p.title, n))
            })
            .collect();
        self.state.discover_hidden_count = self
            .state
            .result_installed
            .iter()
            .filter(|b| **b && !self.state.show_installed_discover)
            .count();
    }

    /// Fetch Modrinth results for the active Discover category and query.
    /// Renders the loading state before blocking on the network call.
    fn fetch_discover_results(&mut self, query: &str) {
        use crate::argus::state::DiscoverTab;
        let tab = self.state.discover_tab;

        // Scope every query to the SELECTED instance: its game version
        // (all categories) and its loader (mods/modpacks). A mod without a
        // build for this MC version simply never appears.
        let (gv, loader) = match self.state.selected_instance.clone() {
            Some(inst) => {
                if matches!(tab, DiscoverTab::Mods | DiscoverTab::Modpacks)
                    && inst.loader == "vanilla"
                {
                    self.state.modrinth_results.clear();
                    self.state.result_installed.clear();
                    self.state.discover_hidden_count = 0;
                    self.state.discover_game_version = inst.game_version.clone();
                    self.state.set_status(format!(
                        "{} require a Fabric/Quilt instance — '{}' is vanilla",
                        tab.label(),
                        inst.name
                    ));
                    return;
                }
                let loader_filter = match tab {
                    DiscoverTab::Mods => Some(inst.loader.clone()),
                    // Modpacks bundle their own loader config — filtering by
                    // the instance's loader hides Forge/Quilt packs that do
                    // support this game version. Version scope is kept.
                    DiscoverTab::Modpacks => None,
                    _ => None,
                };
                (inst.game_version.clone(), loader_filter)
            }
            None => {
                self.state.modrinth_results.clear();
                self.state.result_installed.clear();
                self.state.discover_hidden_count = 0;
                self.state.discover_game_version = String::new();
                self.state.set_status(
                    "No instances yet — create one on HOME to browse content".to_string(),
                );
                return;
            }
        };
        self.state.discover_game_version = gv.clone();
        // Tag this result set with the instance it was scoped to, so later
        // instance switches can detect staleness and re-scope.
        self.state.discover_scoped_instance =
            self.state.selected_instance.as_ref().map(|i| i.id.clone());

        // Draw the loading state before the blocking request so the user
        // sees feedback instead of a frozen frame.
        self.state.set_loading(
            true,
            Some(match query.is_empty() {
                true => format!("Loading {} for MC {} ...", tab.label(), gv),
                false => format!("Searching {} for '{}' (MC {})...", tab.label(), query, gv),
            }),
        );
        let _ = self.renderer.render(&self.state, &self.focus);

        let results = BackendBridge::fetch_discover(tab, query, &gv, loader.as_deref());
        self.state.set_loading(false, None);
        match results {
            Ok(list) => {
                self.state.modrinth_results = list;
                // Hide anything already installed in the selected instance.
                self.classify_installed_results();
                let shown = self.state.modrinth_results.len();
                let hidden = self.state.discover_hidden_count;
                self.state.log(
                    LogLevel::Info,
                    "BACKEND",
                    &format!(
                        "{}: {} shown, {} already installed (hidden){}",
                        tab.label(),
                        shown,
                        hidden,
                        if query.is_empty() {
                            String::new()
                        } else {
                            format!(" for '{}'", query)
                        }
                    ),
                );
                if shown == 0 {
                    self.state.set_status(format!(
                        "No new {} to show{}{}",
                        tab.label().to_lowercase(),
                        if hidden > 0 {
                            format!(" — {} already installed", hidden)
                        } else {
                            String::new()
                        },
                        if query.is_empty() {
                            String::new()
                        } else {
                            format!(" for '{}'", query)
                        }
                    ));
                }
            }
            Err(e) => {
                self.state
                    .set_error(format!("Modrinth fetch failed: {}", e));
            }
        }
        self.rebuild_targets_preserving_focus();
    }

    /// Focus the DISCOVER search bar and start capturing keystrokes.
    fn activate_search(&mut self) {
        if self.state.current_section != Section::Discover {
            return;
        }
        self.focus.set_by_id("disc_search");
        self.state.search_mode = true;
    }

    /// Handle activation of a focused target
    fn handle_activate(&mut self, target_id: &str) {
        self.state.log(
            crate::argus::state::LogLevel::Info,
            "ARGUS",
            &format!("Activated: {}", target_id),
        );

        match target_id {
            "home_launch" => {
                self.home_launch();
            }
            "home_create" => {
                self.home_create();
            }
            "home_play" => {
                self.home_launch();
            }
            "home_open" => {
                self.open_instance_folder();
            }
            "disc_search" => {
                self.activate_search();
            }
            id if id.starts_with("disc_cat_") => {
                if let Ok(idx) = id.strip_prefix("disc_cat_").unwrap_or("").parse::<usize>() {
                    self.switch_discover_category(idx);
                }
            }
            id if id.starts_with("project_") => {
                if let Ok(idx) = id.strip_prefix("project_").unwrap_or("").parse::<usize>() {
                    self.open_install_version_chooser(idx);
                }
            }
            id if id.starts_with("instance_") => {
                if let Some(idx_str) = id.strip_prefix("instance_") {
                    match idx_str {
                        "stop" => self.cmd_stop(),
                        "delete" => self.delete_selected_instance(),
                        "empty" => {
                            self.state
                                .set_status("Press 'c' on HOME to create an instance".to_string());
                        }
                        "select" => {}
                        _ => {
                            if let Ok(idx) = idx_str.parse::<usize>() {
                                let already_selected = self
                                    .state
                                    .selected_instance
                                    .as_ref()
                                    .and_then(|s| {
                                        self.state.instances.iter().position(|i| i.id == s.id)
                                    })
                                    .map(|p| p == idx)
                                    .unwrap_or(false);
                                if already_selected {
                                    // ENTER on an already-selected instance launches it
                                    self.home_launch_index(idx);
                                } else {
                                    self.state.select_instance(idx);
                                    self.state.set_status(format!(
                                        "Selected '{}' — press ENTER again to launch",
                                        self.state.instances[idx].name
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            "settings_memory" => {
                self.open_memory_selector();
            }
            "settings_java" => {
                self.open_java_selector();
            }
            "settings_theme" => {
                self.open_theme_selector();
            }
            "settings_language" => {
                self.open_language_info();
            }
            "settings_optimization" => {
                self.open_optimization_selector();
            }
            "settings_custom_jvm" => {
                self.open_custom_jvm_editor();
            }
            "settings_account" => {
                self.open_account_selector();
            }
            "settings_window" | "settings_java_list" => {
                // Window and Java Installations are read-only (informational)
            }
            id if id.starts_with("settings_") => {
                // Generic settings handler
            }
            id if id.starts_with("server_") => {
                if let Some(idx_str) = id.strip_prefix("server_") {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        self.activate_server(idx);
                    }
                }
            }
            "server_add" => {
                self.open_server_add();
            }
            "screenshots_open" => {
                let dir = BackendBridge::instances_dir()
                    .join(
                        self.state
                            .selected_instance
                            .as_ref()
                            .map(|i| i.id.clone())
                            .unwrap_or_default(),
                    )
                    .join("game")
                    .join("screenshots");
                if open_in_file_manager(&dir) {
                    self.state.set_status(format!(
                        "Opened: {}",
                        dir.display()
                    ));
                } else {
                    self.state.set_error("No screenshots folder yet".to_string());
                }
            }
            id if id.starts_with("screenshot_") => {
                if let Some(idx_str) = id.strip_prefix("screenshot_") {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if let Some(entry) = self.state.screenshots.get(idx) {
                            let _ = open_in_file_manager(&entry.path);
                        }
                    }
                }
            }
            "crashes_copy" => self.copy_selected_crash(),
            "crashes_delete" => self.delete_selected_crash(),
            "crashes_open" => {
                if let Some(cr) = self.state.crash_reports.first() {
                    if let Some(parent) = cr.path.parent() {
                        let _ = open_in_file_manager(parent);
                    }
                }
            }
"logs_live" => {
                self.state.live_log_view = !self.state.live_log_view;
self.state.set_status(if self.state.live_log_view {
                    "Live log tail enabled — Shift+L to disable".to_string()
                } else {
                    "Live log tail disabled".to_string()
                });
                self.rebuild_targets_preserving_focus();
            }
            _ => {}
        }
    }

    /// ENTER on a DISCOVER result: fetch its compatible builds and open the
    /// version chooser instead of blindly installing the newest (which may
    /// be an alpha incompatible with the user's other mods).
    fn open_install_version_chooser(&mut self, idx: usize) {
        use crate::argus::state::DiscoverTab;

        let Some(project) = self.state.modrinth_results.get(idx).cloned() else {
            return;
        };
        let content_type = match self.state.discover_tab {
            DiscoverTab::Mods => "mod",
            DiscoverTab::Modpacks => "modpack",
            DiscoverTab::Shaders => "shader",
            DiscoverTab::ResourcePacks => "resourcepack",
        };
        let Some(inst) = self.state.selected_instance.clone() else {
            self.state
                .set_status("No instances yet — create one on HOME first".to_string());
            return;
        };
        let needs_loader = matches!(content_type, "mod" | "modpack");
        if needs_loader && inst.loader == "vanilla" {
            self.state.set_error(format!(
                "'{}' needs a Fabric/Quilt instance — '{}' is vanilla",
                project.title, inst.name
            ));
            return;
        }
        let loader_filter = if needs_loader {
            Some(inst.loader.as_str())
        } else {
            None
        };

        self.state.set_loading(
            true,
            Some(format!("Fetching {} versions...", project.title)),
        );
        let _ = self.renderer.render(&self.state, &self.focus);
        let scoped = BackendBridge::list_project_versions_scoped(
            &project.id,
            loader_filter,
            &inst.game_version,
        );
        self.state.set_loading(false, None);

        match scoped {
            Ok(list) if !list.is_empty() => {
                let rows = list
                    .iter()
                    .map(|v| {
                        (
                            v.id.clone(),
                            format!(
                                "{:<34} [{:^6}]",
                                truncate_label(&v.version_number, 34),
                                if v.version_type.is_empty() {
                                    "?"
                                } else {
                                    &v.version_type
                                }
                            ),
                        )
                    })
                    .collect();
                self.state.pending_install = Some(crate::argus::state::PendingInstall {
                    project_id: project.id.clone(),
                    title: project.title.clone(),
                    content_type: content_type.to_string(),
                    instance_id: Some(inst.id.clone()),
                    rows,
                });
                self.state.install_version_index = 0;
            }
            Ok(_) => {
                self.state.set_error(format!(
                    "No builds of '{}' for MC {} {}",
                    project.title,
                    inst.game_version,
                    loader_filter
                        .map(|l| format!("({}) ", l))
                        .unwrap_or_default()
                ));
            }
            Err(e) => self.state.set_error(e),
        }
    }

    /// Keys inside the per-mod version chooser.
    fn handle_install_version_input(&mut self, key: KeyEvent) {
        let n = self
            .state
            .pending_install
            .as_ref()
            .map(|p| p.rows.len())
            .unwrap_or(0);
        match key.code {
            KeyCode::Esc => {
                self.state.pending_install = None;
            }
            KeyCode::Up => {
                if self.state.install_version_index > 0 {
                    self.state.install_version_index -= 1;
                }
            }
            KeyCode::Down => {
                if self.state.install_version_index + 1 < n {
                    self.state.install_version_index += 1;
                }
            }
            KeyCode::PageUp => {
                self.state.install_version_index =
                    self.state.install_version_index.saturating_sub(10);
            }
            KeyCode::PageDown => {
                if n > 0 {
                    self.state.install_version_index =
                        (self.state.install_version_index + 10).min(n - 1);
                }
            }
            KeyCode::Home => self.state.install_version_index = 0,
            KeyCode::End => {
                if n > 0 {
                    self.state.install_version_index = n - 1;
                }
            }
            KeyCode::Enter => {
                let Some(pi) = self.state.pending_install.clone() else {
                    return;
                };
                let Some((vid, _)) = pi.rows.get(self.state.install_version_index) else {
                    return;
                };
                let vid = vid.clone();
                self.state.pending_install = None;
                self.perform_install(&pi, Some(&vid));
            }
            _ => {}
        }
    }

    /// Download + record + refresh for a pending install.
    fn perform_install(
        &mut self,
        pi: &crate::argus::state::PendingInstall,
        pinned_version_id: Option<&str>,
    ) {
        self.state
            .set_loading(true, Some(format!("Installing {}...", pi.title)));
        let _ = self.renderer.render(&self.state, &self.focus);

        match BackendBridge::install_project_version(
            &pi.project_id,
            &pi.title,
            &pi.content_type,
            pi.instance_id.as_deref(),
            pinned_version_id,
        ) {
            Ok((filename, version_id)) => {
                self.state.set_loading(false, None);
                // Record the install so DISCOVER hides this project from now on.
                if let Some(iid) = pi.instance_id.as_deref() {
                    BackendBridge::record_install(
                        iid,
                        &pi.project_id,
                        version_id.as_deref(),
                        &filename,
                        &pi.content_type,
                    );
                }
                self.state.set_status(format!("Installed: {}", filename));
                self.state.log(
                    LogLevel::Info,
                    "BACKEND",
                    &format!(
                        "Installed '{}' ({}) → {}",
                        pi.title, pi.content_type, filename
                    ),
                );
                // Refresh installed content listing, then RELOAD the result
                // list so fresh content fills the gap instead of shrinking
                // the old one. Stay in the results pane.
                self.state.installed_content =
                    BackendBridge::list_installed_content(pi.instance_id.as_deref());
                self.fetch_discover_results("");
                self.enter_results_pane();
            }
            Err(e) => {
                self.state.set_loading(false, None);
                self.state.set_error(format!("Install failed: {}", e));
            }
        }
    }

    /// Remove the focused installed content file (MODS section).
    fn remove_focused_content(&mut self) {
        let Some(inst) = self.state.selected_instance.clone() else {
            self.state.set_status("No instance selected".to_string());
            return;
        };
        // The focused target id encodes the row: installed_{i}
        let Some(cur) = self.focus.current() else {
            return;
        };
        let Some(idx) = cur
            .id
            .strip_prefix("installed_")
            .and_then(|s| s.parse::<usize>().ok())
        else {
            return;
        };
        let Some(item) = self.state.installed_content.get(idx).cloned() else {
            return;
        };

        match BackendBridge::remove_installed_content(&inst.id, &item.path) {
            Ok(()) => {
                self.state.log(
                    LogLevel::Info,
                    "BACKEND",
                    &format!("Removed {} '{}'", item.kind.to_lowercase(), item.name),
                );
                self.state.set_status(format!("Removed {}", item.name));
                // Rescan + drop the stale focus target, clamped to the list.
                self.state.installed_content =
                    BackendBridge::list_installed_content(Some(&inst.id));
                let max = self.state.installed_content.len();
                if max == 0 {
                    self.focus.set(nav_count());
                } else if idx >= max {
                    self.focus.set_by_id(&format!("installed_{}", max - 1));
                }
            }
            Err(e) => self.state.set_error(e),
        }
    }

    /// Reinstall the latest version of the updatable mod at `idx`.
    #[allow(dead_code)]
    fn update_mod_at_index(&mut self, idx: usize) {
        let Some(updatable) = self.state.updatable_mods.get(idx).cloned() else {
            return;
        };
        let Some(inst) = self.state.selected_instance.clone() else {
            self.state.set_status("No instance selected".to_string());
            return;
        };

        self.state
            .set_loading(true, Some(format!("Updating {}...", updatable.title)));
        let _ = self.renderer.render(&self.state, &self.focus);

        let content_type = match updatable.content_type.as_str() {
            "modpack" => "modpack",
            "resourcepack" => "resourcepack",
            "shader" => "shader",
            _ => "mod",
        };

        let pi = crate::argus::state::PendingInstall {
            project_id: updatable.project_id.clone(),
            title: updatable.title.clone(),
            content_type: content_type.to_string(),
            instance_id: Some(inst.id.clone()),
            rows: vec![(
                updatable.latest_version_id.clone(),
                updatable.latest_version.clone(),
            )],
        };

        self.perform_install(&pi, Some(updatable.latest_version_id.as_str()));
    }

    /// Launch the selected/default instance via real backend
    fn home_launch(&mut self) {
        let index = self
            .state
            .selected_instance
            .as_ref()
            .and_then(|sel| self.state.instances.iter().position(|i| i.id == sel.id))
            .unwrap_or(0);
        self.home_launch_index(index);
    }

    /// Launch the instance at `index` via the background launch pipeline.
    /// Returns immediately; progress arrives through poll_launch_events().
    fn home_launch_index(&mut self, index: usize) {
        if index >= self.state.instances.len() {
            self.state.set_error("No instances available".to_string());
            return;
        }
        if self.tracker.launch_events.is_some() {
            self.state
                .set_status("A launch is already in progress — watch the bar below".to_string());
            return;
        }
        match self.state.runtime_state {
            RuntimeState::Starting | RuntimeState::Running | RuntimeState::Stopping => {
                self.state.set_status(
                    "Minecraft is already starting/running — wait or press Stop".to_string(),
                );
                return;
            }
            _ => {}
        }

        let instance = self.state.instances[index].clone();
        self.state.runtime_state = RuntimeState::Starting;
        self.state.log(
            LogLevel::Info,
            "ARGUS",
            &format!("Launching instance: {}", instance.name),
        );
        self.state
            .set_loading(true, Some("Preparing launch...".to_string()));
        let _ = self.renderer.render(&self.state, &self.focus);

        BackendBridge::spawn_launch(&instance, &mut self.state, &mut self.tracker);
    }

    /// Launch an instance by ID via the background launch pipeline.
    pub fn run_with_instance(&mut self, instance_id: &str) -> anyhow::Result<()> {
        let idx = self
            .state
            .instances
            .iter()
            .position(|i| i.id == instance_id)
            .ok_or_else(|| anyhow::anyhow!("Instance not found: {}", instance_id))?;
        self.home_launch_index(idx);
        Ok(())
    }

    /// Drain events from the in-flight background launch.
    fn poll_launch_events(&mut self) {
        if self.tracker.launch_events.is_none() {
            return;
        }
        // Take the receiver out so terminal events can clear it cleanly.
        let rx = match self.tracker.launch_events.take() {
            Some(rx) => rx,
            None => return,
        };
        let mut finished = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                LaunchEvent::Progress(msg) => {
                    self.state.set_loading(true, Some(msg));
                }
                LaunchEvent::Log(level, msg) => {
                    self.state.log(level, "BACKEND", &msg);
                }
                LaunchEvent::Launched(child) => {
                    let pid = child.id();
                    self.tracker.set_process(child);
                    self.state.runtime_state = RuntimeState::Running;
                    self.state.set_loading(false, None);
                    self.state.log(
                        LogLevel::Info,
                        "BACKEND",
                        &format!(
                            "Minecraft launched (PID: {}) — window may take a minute on first run",
                            pid
                        ),
                    );
                    self.state
                        .set_status(format!("Launched (PID {}) — see LOGS for output", pid));
                    finished = true;
                }
                LaunchEvent::Failed(e) => {
                    finished = true;
                    self.state.set_loading(false, None);
                    self.state.runtime_state = RuntimeState::Error(e.clone());
                    self.state.set_error(format!("Launch failed: {}", e));
                }
            }
        }
        if !finished {
            // Still launching — put the receiver back for next tick.
            self.tracker.launch_events = Some(rx);
        } else {
            // Register/unregister the Stop button for the new state.
            self.rebuild_targets_preserving_focus();
        }
    }

    /// Hide the TUI and let the running Minecraft process own the terminal.
    ///
    /// Called the moment the game transitions to `Running`: we take the child
    /// process out of the tracker, tear down the alternate screen / raw mode,
    /// and block on the process. The game's console output is captured into a
    /// buffer (NOT printed to the terminal — the user does not want the game
    /// log streamed to the console) and replayed into the ARGUS LOGS view
    /// when the launcher returns. When the process exits — whether the user
    /// closed it, it crashed, or it was force-killed — we rebuild the TUI and
    /// resume exactly where we left off.
    fn enter_game_mode(&mut self) -> anyhow::Result<()> {
        let mut child = match self.tracker.take_child() {
            Some(c) => c,
            None => return Ok(()),
        };
        let stdout_rx = self.tracker.take_stdout_rx();
        let stderr_rx = self.tracker.take_stderr_rx();

        // Tear down the TUI and hide the console window so only the
        // game is visible while it runs.
        self.renderer.deinit()?;
        win_console::hide();

        let pid_label = self
            .tracker
            .pid()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        println!(
            "\r\n[ARGUS] Minecraft launched (PID {}) — launcher hidden.\r\n         It will reappear automatically when the game closes.\r\n",
            pid_label
        );

        // Write any captured game output to `<instance>/logs/argus-live.log`
        // so it can be inspected via the LOGS section's live tail view. On
        // Windows, the console-hide above means we cannot render the lines
        // in-TUI; the file is the only inspection channel.
        let log_buffer: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        let live_log_path = self
            .state
            .selected_instance
            .as_ref()
            .map(|inst| {
                crate::platform::Paths::new()
                    .instances_dir()
                    .join(&inst.id)
                    .join("logs")
                    .join("argus-live.log")
            });
        if let Some(ref path) = live_log_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Truncate previous run
            let _ = std::fs::write(path, "");
        }
        if let Some(rx) = stdout_rx {
            let buf = Arc::clone(&log_buffer);
            let path = live_log_path.clone();
            handles.push(std::thread::spawn(move || {
                while let Ok(line) = rx.recv() {
                    if let Some(ref p) = path {
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(p)
                        {
                            use std::io::Write;
                            let _ = writeln!(f, "{}", line);
                        }
                    }
                    if let Ok(mut b) = buf.lock() {
                        b.push(line);
                    }
                }
            }));
        }
        if let Some(rx) = stderr_rx {
            let buf = Arc::clone(&log_buffer);
            let path = live_log_path.clone();
            handles.push(std::thread::spawn(move || {
                while let Ok(line) = rx.recv() {
                    if let Some(ref p) = path {
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(p)
                        {
                            use std::io::Write;
                            let _ = writeln!(f, "{}", line);
                        }
                    }
                    if let Ok(mut b) = buf.lock() {
                        b.push(line);
                    }
                }
            }));
        }

        // Block until the game process exits for ANY reason.
        let status = child.wait();
        for h in handles {
            let _ = h.join();
        }

        // Replay the captured game output into the LOGS view (keep the most
        // recent lines to bound memory on very chatty sessions).
        let captured = Arc::try_unwrap(log_buffer)
            .map(|m| m.into_inner().unwrap_or_default())
            .unwrap_or_default();
        let start = captured.len().saturating_sub(2000);
        for line in &captured[start..] {
            self.state.log(LogLevel::Info, "Minecraft", line);
        }

        let exit_msg = match &status {
            Ok(s) => match s.code() {
                Some(0) | None => "Minecraft closed cleanly".to_string(),
                Some(c) => format!("Minecraft exited with code {}", c),
            },
            Err(e) => format!("Failed to wait for Minecraft: {}", e),
        };

        // Restore the console window, then rebuild the TUI and resume the loop.
        win_console::show();
        self.renderer = Renderer::init()?;
        self.state.runtime_state = RuntimeState::Stopped;
        self.state.set_loading(false, None);
        self.tracker.clear_after_game();
        self.state.live_log_path = live_log_path;
        self.state.set_status(exit_msg.clone());
        self.state.log(
            LogLevel::Info,
            "ARGUS",
            &format!("{} — launcher restored", exit_msg),
        );
        self.rebuild_targets_preserving_focus();
        Ok(())
    }

    /// Delete the currently selected instance (persisted).
    fn delete_selected_instance(&mut self) {
        let Some(sel) = self.state.selected_instance.clone() else {
            self.state
                .set_status("No instance selected — ↑↓ to pick one".to_string());
            return;
        };
        let name = sel.name.clone();
        if BackendBridge::delete_instance(&sel.id) {
            self.state.log(
                LogLevel::Info,
                "BACKEND",
                &format!("Deleted instance '{}' (persisted)", name),
            );
            self.state.set_status(format!("Deleted '{}'", name));
            self.state.selected_instance = None;
            self.refresh_data();
            self.rebuild_targets_preserving_focus();
        } else {
            self.state
                .set_error(format!("Failed to delete instance '{}'", name));
        }
    }

    /// Stop the running Minecraft process via real backend
    fn cmd_stop(&mut self) {
        self.state.runtime_state = RuntimeState::Stopping;
        self.state.log(
            crate::argus::state::LogLevel::Info,
            "ARGUS",
            "Stopping Minecraft...",
        );

        let stopped = BackendBridge::stop_instance(&mut self.state, &mut self.tracker);
        if stopped {
            self.state.runtime_state = RuntimeState::Stopped;
            self.state.log(
                crate::argus::state::LogLevel::Info,
                "BACKEND",
                "Minecraft stopped",
            );
        } else {
            self.state.runtime_state = RuntimeState::Error("Failed to stop process".to_string());
            self.state
                .set_error("Failed to stop Minecraft process".to_string());
        }
    }

    /// Create a new instance — opens the loader picker (vanilla/fabric/quilt)
    fn home_create(&mut self) {
        self.state.loader_selector_index = 0;
        self.state.loader_selector_open = true;
    }

    /// Open the selected instance's folder in the system file manager. Falls
    /// back to the instances root when no instance is selected.
    fn open_instance_folder(&mut self) {
        let dir = BackendBridge::instances_dir();
        let target = match &self.state.selected_instance {
            Some(inst) => dir.join(&inst.id),
            None => dir,
        };
        if !target.exists() {
            self.state
                .set_error(format!("Folder not found: {}", target.display()));
            return;
        }
        if open_in_file_manager(&target) {
            self.state
                .set_status(format!("Opened folder: {}", target.display()));
            self.state.log(
                LogLevel::Info,
                "ARGUS",
                &format!("Opened folder: {}", target.display()),
            );
        } else {
            self.state
                .set_error(format!("Failed to open folder: {}", target.display()));
        }
    }

    // ===== Server Management =====

    fn activate_server(&mut self, idx: usize) {
        let Some(server) = self.state.servers.get(idx).cloned() else {
            return;
        };
        let key = server.id.clone();
        let addr = server.address.clone();
        if self.state.server_pinging {
            return;
        }
        self.state.server_pinging = true;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = crate::servers::ping_server(&addr);
            let _ = tx.send(result);
        });
        self.state.log(
            LogLevel::Info,
            "SERVERS",
            &format!("Pinging {}...", server.address),
        );
        let pings = &mut self.state.server_pings;
        match rx.recv() {
            Ok(Ok(info)) => {
                pings.insert(key, info);
            }
            Ok(Err(e)) => {
                self.state.set_error(format!("Ping failed: {}", e));
            }
            Err(_) => {}
        }
        self.state.server_pinging = false;
        self.rebuild_targets_preserving_focus();
    }

    fn open_server_add(&mut self) {
        self.state.server_add_open = true;
        self.state.server_add_name.clear();
        self.state.server_add_address.clear();
    }

    /// Copy the most recent crash report's text to the clipboard. Falls back
    /// to writing `<data_local>/.crash-clipboard.txt` when no clipboard tool
    /// is available (Linux without xclip/xsel).
    fn copy_selected_crash(&mut self) {
        let Some(cr) = self.state.crash_reports.first().cloned() else {
            self.state
                .set_status("No crash report selected".to_string());
            return;
        };
        let text = std::fs::read_to_string(&cr.path).unwrap_or_default();
        if copy_to_clipboard(&text) {
            self.state
                .set_status("Copied crash report to clipboard".to_string());
            self.state.log(
                LogLevel::Info,
                "CRASHES",
                "Copied selected crash report to clipboard",
            );
        } else {
            let fallback = crate::platform::Paths::new()
                .data_local
                .join(".crash-clipboard.txt");
            let _ = std::fs::write(&fallback, &text);
            self.state.set_status(format!(
                "Clipboard unavailable — wrote to {}",
                fallback.display()
            ));
        }
    }

    fn delete_selected_crash(&mut self) {
        let Some(cr) = self.state.crash_reports.first().cloned() else {
            return;
        };
        if std::fs::remove_file(&cr.path).is_ok() {
            self.state.log(
                LogLevel::Info,
                "CRASHES",
                &format!("Deleted {}", cr.path.display()),
            );
            self.refresh_data();
            self.rebuild_targets_preserving_focus();
        } else {
            self.state
                .set_error(format!("Failed to delete {}", cr.path.display()));
        }
    }

    // ===== Worlds =====

    fn focused_world(&self) -> Option<String> {
        let id = self.focus.current().map(|t| t.id.clone())?;
        if !id.starts_with("world_") {
            return None;
        }
        let idx = id.trim_start_matches("world_").parse::<usize>().ok()?;
        self.state.worlds.get(idx).cloned()
    }

    fn backup_focused_world(&mut self) {
        let Some(inst) = self.state.selected_instance.clone() else {
            self.state
                .set_status("Select an instance first (↑↓ on INSTANCES)".to_string());
            return;
        };
        let Some(world) = self.focused_world() else {
            self.state.set_status("No world focused".to_string());
            return;
        };
        match crate::worlds::backup_world(&inst.id, &world) {
            Ok(path) => {
                self.state.log(
                    LogLevel::Info,
                    "WORLDS",
                    &format!("Backed up '{}' to {}", world, path.display()),
                );
                self.state
                    .set_status(format!("Backed up to {}", path.display()));
            }
            Err(e) => self.state.set_error(format!("Backup failed: {}", e)),
        }
    }

    fn open_focused_world_folder(&mut self) {
        let Some(inst) = self.state.selected_instance.clone() else {
            return;
        };
        let Some(world) = self.focused_world() else {
            return;
        };
        let dir = crate::platform::Paths::new()
            .instances_dir()
            .join(inst.id)
            .join("saves")
            .join(&world);
        let _ = open_in_file_manager(&dir);
    }

    fn open_worlds_save_folder(&mut self) {
        let Some(inst) = self.state.selected_instance.clone() else {
            return;
        };
        let dir = crate::platform::Paths::new()
            .instances_dir()
            .join(inst.id)
            .join("saves");
        let _ = open_in_file_manager(&dir);
    }

    fn delete_focused_world(&mut self) {
        let Some(inst) = self.state.selected_instance.clone() else {
            return;
        };
        let Some(world) = self.focused_world() else {
            return;
        };
        match crate::worlds::delete_world(&inst.id, &world) {
            Ok(()) => {
                self.state
                    .log(LogLevel::Info, "WORLDS", &format!("Deleted '{}'", world));
                self.refresh_data();
                self.rebuild_targets_preserving_focus();
            }
            Err(e) => self.state.set_error(format!("Delete failed: {}", e)),
        }
    }

    // ===== Settings Editor Methods =====

    /// Open the memory selector for editing default memory
    fn open_memory_selector(&mut self) {
        let settings = BackendBridge::get_settings();
        // Position the selector at the currently selected value
        let presets = crate::argus::state::MEMORY_PRESETS;
        let current = settings.default_memory;
        let selected_idx = presets.iter().position(|&m| m == current).unwrap_or(1); // default to 4096 if not found

        self.state.settings_edit_mode = SettingsEditMode::MemorySelector;
        self.state.settings_edit_index = selected_idx;
        self.state.log(
            LogLevel::Info,
            "ARGUS",
            &format!(
                "Memory selector opened (current: {} MB, {} presets)",
                current,
                presets.len()
            ),
        );
    }

    /// Open the Java selector for choosing Java path
    fn open_java_selector(&mut self) {
        self.state.settings_edit_mode = SettingsEditMode::JavaSelector;
        self.state.settings_edit_index = 0; // Start at "Auto-detect"
        self.state
            .log(LogLevel::Info, "ARGUS", "Java selector opened");
    }

    /// Open the theme selector
    fn open_theme_selector(&mut self) {
        let settings = BackendBridge::get_settings();
        let themes = &self.state.theme_options;
        let selected_idx = themes
            .iter()
            .position(|t| t == &settings.theme)
            .unwrap_or(0);

        self.state.settings_edit_mode = SettingsEditMode::ThemeSelector;
        self.state.settings_edit_index = selected_idx;
        self.state.log(
            LogLevel::Info,
            "ARGUS",
            &format!("Theme selector opened (current: {})", settings.theme),
        );
    }

    /// Open the language info view
    fn open_language_info(&mut self) {
        self.state.settings_edit_mode = SettingsEditMode::LanguageInfo;
        self.state.settings_edit_index = 0;
        self.state.log(
            LogLevel::Info,
            "ARGUS",
            "Language: only English is currently available",
        );
    }

    /// Open the optimization profile selector
    fn open_optimization_selector(&mut self) {
        let settings = BackendBridge::get_settings();
        let profiles = crate::minecraft::optimization::OptimizationProfile::all();
        let selected_idx = profiles
            .iter()
            .position(|p| *p == settings.optimization_profile)
            .unwrap_or(1); // default to Mid if not found

        self.state.settings_edit_mode = SettingsEditMode::OptimizationSelector;
        self.state.settings_edit_index = selected_idx;
        self.state.log(
            LogLevel::Info,
            "ARGUS",
            &format!(
                "Optimization selector opened (current: {})",
                settings.optimization_profile.as_str()
            ),
        );
    }

    /// Open the custom JVM args text editor
    fn open_custom_jvm_editor(&mut self) {
        let settings = BackendBridge::get_settings();
        self.state.custom_jvm_input = settings.custom_jvm_args.join(" ");
        self.state.settings_edit_mode = SettingsEditMode::CustomJvmEditor;
        self.state.settings_edit_index = 0;
        self.state.log(
            LogLevel::Info,
            "ARGUS",
            "Custom JVM args editor opened (type args, ENTER to save, ESC to cancel)",
        );
    }

    /// Handle Up key in settings edit mode
    fn settings_edit_up(&mut self) {
        match &self.state.settings_edit_mode {
            SettingsEditMode::MemorySelector => {
                let presets = crate::argus::state::MEMORY_PRESETS;
                if self.state.settings_edit_index > 0 {
                    self.state.settings_edit_index -= 1;
                }
                let selected = presets[self.state.settings_edit_index];
                self.state.log(
                    LogLevel::Info,
                    "ARGUS",
                    &format!("Memory: {} MB selected", selected),
                );
            }
            SettingsEditMode::JavaSelector => {
                if self.state.settings_edit_index > 0 {
                    self.state.settings_edit_index -= 1;
                }
                self.settings_java_preview();
            }
            SettingsEditMode::ThemeSelector => {
                if self.state.settings_edit_index > 0 {
                    self.state.settings_edit_index -= 1;
                }
                let theme = &self.state.theme_options[self.state.settings_edit_index];
                self.state
                    .log(LogLevel::Info, "ARGUS", &format!("Theme: {}", theme));
            }
            SettingsEditMode::LanguageInfo => {
                // Language info is read-only, just navigate
            }
            SettingsEditMode::OptimizationSelector => {
                if self.state.settings_edit_index > 0 {
                    self.state.settings_edit_index -= 1;
                }
                let profile = crate::minecraft::optimization::OptimizationProfile::all()
                    [self.state.settings_edit_index];
                self.state.log(
                    LogLevel::Info,
                    "ARGUS",
                    &format!("Optimization: {}", profile.as_str()),
                );
            }
            SettingsEditMode::None => {}
            SettingsEditMode::CustomJvmEditor => {}
        }
    }

    /// Handle Down key in settings edit mode
    fn settings_edit_down(&mut self) {
        match &self.state.settings_edit_mode {
            SettingsEditMode::MemorySelector => {
                let presets = crate::argus::state::MEMORY_PRESETS;
                if self.state.settings_edit_index < presets.len() - 1 {
                    self.state.settings_edit_index += 1;
                }
                let selected = presets[self.state.settings_edit_index];
                self.state.log(
                    LogLevel::Info,
                    "ARGUS",
                    &format!("Memory: {} MB selected", selected),
                );
            }
            SettingsEditMode::JavaSelector => {
                // Java installations + "Auto-detect" at index 0
                let java_count = self.state.java_installations.len();
                let max = java_count + 1; // +1 for auto-detect
                if max > 0 && self.state.settings_edit_index < max - 1 {
                    self.state.settings_edit_index += 1;
                }
                self.settings_java_preview();
            }
            SettingsEditMode::ThemeSelector => {
                if self.state.settings_edit_index < self.state.theme_options.len() - 1 {
                    self.state.settings_edit_index += 1;
                }
                let theme = &self.state.theme_options[self.state.settings_edit_index];
                self.state
                    .log(LogLevel::Info, "ARGUS", &format!("Theme: {}", theme));
            }
            SettingsEditMode::LanguageInfo => {
                // Language info is read-only
            }
            SettingsEditMode::OptimizationSelector => {
                let profiles = crate::minecraft::optimization::OptimizationProfile::all();
                if self.state.settings_edit_index < profiles.len() - 1 {
                    self.state.settings_edit_index += 1;
                }
                let profile = profiles[self.state.settings_edit_index];
                self.state.log(
                    LogLevel::Info,
                    "ARGUS",
                    &format!("Optimization: {}", profile.as_str()),
                );
            }
            SettingsEditMode::None => {}
            SettingsEditMode::CustomJvmEditor => {}
        }
    }

    /// Preview the selected Java in the selector
    fn settings_java_preview(&mut self) {
        if self.state.settings_edit_index == 0 {
            self.state
                .log(LogLevel::Info, "ARGUS", "Java: Auto-detect selected");
        } else {
            let idx = self.state.settings_edit_index - 1;
            if idx < self.state.java_installations.len() {
                let java = &self.state.java_installations[idx];
                let version = java
                    .version
                    .as_ref()
                    .map(|v| format!("Java {}", v.major))
                    .unwrap_or("Unknown".to_string());
                self.state.log(
                    LogLevel::Info,
                    "ARGUS",
                    &format!("Java: {} — {}", version, java.path.to_string_lossy()),
                );
            }
        }
    }

    /// Cancel the current settings edit and return to normal mode
    fn cancel_settings_edit(&mut self) {
        self.state.settings_edit_mode = SettingsEditMode::None;
        self.state.settings_edit_index = 0;
        self.state
            .log(LogLevel::Info, "ARGUS", "Settings edit cancelled");
    }

    /// Apply the current settings edit (Enter in edit mode)
    fn apply_settings_edit(&mut self) {
        match &self.state.settings_edit_mode {
            SettingsEditMode::MemorySelector => {
                let presets = crate::argus::state::MEMORY_PRESETS;
                let selected = presets[self.state.settings_edit_index];
                if BackendBridge::set_default_memory(selected) {
                    self.state.log(
                        LogLevel::Info,
                        "BACKEND",
                        &format!("Default memory set to {} MB (persisted)", selected),
                    );
                } else {
                    self.state.log(
                        LogLevel::Error,
                        "BACKEND",
                        &format!("Failed to save memory setting: {} MB", selected),
                    );
                }
                self.state.settings_edit_mode = SettingsEditMode::None;
            }
            SettingsEditMode::JavaSelector => {
                if self.state.settings_edit_index == 0 {
                    // Auto-detect
                    if BackendBridge::set_java_path(None) {
                        self.state.log(
                            LogLevel::Info,
                            "BACKEND",
                            "Java path set to Auto-detect (persisted)",
                        );
                    } else {
                        self.state.log(
                            LogLevel::Error,
                            "BACKEND",
                            "Failed to save Java path setting",
                        );
                    }
                } else {
                    let idx = self.state.settings_edit_index - 1;
                    if idx < self.state.java_installations.len() {
                        let path = self.state.java_installations[idx]
                            .path
                            .to_string_lossy()
                            .to_string();
                        if BackendBridge::set_java_path(Some(path.clone())) {
                            self.state.log(
                                LogLevel::Info,
                                "BACKEND",
                                &format!("Java path set to {} (persisted)", path),
                            );
                        } else {
                            self.state.log(
                                LogLevel::Error,
                                "BACKEND",
                                "Failed to save Java path setting",
                            );
                        }
                    }
                }
                self.state.settings_edit_mode = SettingsEditMode::None;
            }
            SettingsEditMode::ThemeSelector => {
                let theme_name = self.state.theme_options[self.state.settings_edit_index].clone();
                if BackendBridge::set_theme(&theme_name) {
                    // set_theme applies the palette globally; log confirmation
                    self.state.log(
                        LogLevel::Info,
                        "BACKEND",
                        &format!("Theme set to {} (persisted, applied)", theme_name),
                    );
                } else {
                    self.state.log(
                        LogLevel::Error,
                        "BACKEND",
                        &format!("Failed to save theme: {}", theme_name),
                    );
                }
                self.state.settings_edit_mode = SettingsEditMode::None;
            }
            SettingsEditMode::LanguageInfo => {
                self.state.log(
                    LogLevel::Info,
                    "ARGUS",
                    "Language: only English is currently available",
                );
                self.state.settings_edit_mode = SettingsEditMode::None;
            }
            SettingsEditMode::OptimizationSelector => {
                let profiles = crate::minecraft::optimization::OptimizationProfile::all();
                let selected = profiles[self.state.settings_edit_index];
                if selected == crate::minecraft::optimization::OptimizationProfile::Custom {
                    self.open_custom_jvm_editor();
                    return;
                } else if BackendBridge::set_optimization_profile(selected) {
                    self.state.log(
                        LogLevel::Info,
                        "BACKEND",
                        &format!(
                            "Optimization profile set to {} (persisted)",
                            selected.as_str()
                        ),
                    );
                } else {
                    self.state.log(
                        LogLevel::Error,
                        "BACKEND",
                        &format!("Failed to save optimization profile: {}", selected.as_str()),
                    );
                }
                self.state.settings_edit_mode = SettingsEditMode::None;
            }
            SettingsEditMode::None => {}
            SettingsEditMode::CustomJvmEditor => {}
        }
        self.state.settings_edit_index = 0;
    }

    /// Open the command prompt
    fn open_command_prompt(&mut self) {
        self.state.command_prompt_active = true;
        self.state.command_input.clear();
        self.state.history_position = 0;
    }

    /// Execute a command
    fn execute_command(&mut self, command: &str) {
        let result = CommandManager::execute(command, &mut self.state, &mut self.tracker);
        match &result {
            CommandResult::Navigate(section) => {
                self.navigate_to_section(*section);
            }
            CommandResult::Quit => {
                self.state.should_quit = true;
            }
            CommandResult::Success(Some(msg)) => {
                self.state.set_status(msg.clone());
            }
            CommandResult::Error(e) => {
                self.state.set_error(e.clone());
            }
            CommandResult::Output(text) => {
                for line in text.lines() {
                    self.state.log(LogLevel::Info, "CMD", line);
                }
            }
            CommandResult::Help => {
                for line in CommandManager::help_text().lines() {
                    self.state.log(LogLevel::Info, "CMD", line);
                }
            }
            _ => {}
        }
        self.refresh_data();
        self.rebuild_targets_preserving_focus();
    }

    /// Exit the application
    fn exit(&mut self) {
        self.state.should_quit = true;
    }
}

impl Default for ArgusApp {
    fn default() -> Self {
        Self::new().expect("Failed to initialize ARGUS")
    }
}

/// Copy text to the system clipboard via `arboard`. Returns false if no
/// clipboard tool is available (e.g. Linux without xclip/xsel/arboard's
/// required libs).
fn copy_to_clipboard(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut cb) => match cb.set_text(text.to_string()) {
            Ok(()) => true,
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Hide/show the Windows console window during game mode so that launching
/// an instance does not leave the terminal window visible behind the game.
mod win_console {
    #[cfg(windows)]
    mod imp {
        use std::ffi::c_void;
        use std::os::raw::c_int;

        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetConsoleWindow() -> *mut c_void;
            fn ShowWindow(hwnd: *mut c_void, n_cmd_show: c_int) -> c_int;
        }

        const SW_HIDE: c_int = 0;
        const SW_SHOW: c_int = 5;

        /// Returns the console window handle without taking ownership.
        fn console_hwnd() -> *mut c_void {
            unsafe { GetConsoleWindow() }
        }

        pub fn hide() {
            let hwnd = console_hwnd();
            if !hwnd.is_null() {
                unsafe { ShowWindow(hwnd, SW_HIDE) };
            }
        }

        pub fn show() {
            let hwnd = console_hwnd();
            if !hwnd.is_null() {
                unsafe { ShowWindow(hwnd, SW_SHOW) };
            }
        }
    }

    #[cfg(not(windows))]
    mod imp {
        pub fn hide() {}
        pub fn show() {}
    }

    pub use imp::{hide, show};
}

/// Open a directory (or file) in the OS file manager. Cross-platform best
/// effort; on Windows this reveals the path in Explorer.
fn open_in_file_manager(path: &Path) -> bool {
    #[cfg(windows)]
    {
        // Prefer `explorer`; fall back to `cmd /c start` which reliably handles
        // paths containing spaces and special characters.
        if std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .is_ok()
        {
            return true;
        }
        let arg = path.to_string_lossy().to_string();
        if std::process::Command::new("cmd")
            .args(["/c", "start", "", &arg])
            .spawn()
            .is_ok()
        {
            return true;
        }
        false
    }
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener).arg(path).spawn().is_ok()
    }
}

#[cfg(test)]
mod tests {
    /// Version picker filter: case-insensitive substring, empty = all.
    #[test]
    fn test_filter_versions_substring() {
        let all = vec![
            "26.2".to_string(),
            "1.21.1".to_string(),
            "1.21.5".to_string(),
            "1.20.1".to_string(),
        ];
        let f = |needle: &str| -> Vec<String> {
            let n = needle.to_lowercase();
            all.iter()
                .filter(|v| n.is_empty() || v.to_lowercase().contains(&n))
                .cloned()
                .collect()
        };
        assert_eq!(f("").len(), 4);
        assert_eq!(f("1.21"), vec!["1.21.1", "1.21.5"]);
        assert_eq!(f("26"), vec!["26.2"]);
        assert!(f("zzz").is_empty());
    }
}
