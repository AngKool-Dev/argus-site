//! UI widgets for ARGUS terminal interface.
//!
//! Each widget is a self-contained drawing function that takes a frame,
//! area, and state, and draws into the frame using ratatui.
//!
//! All colors come from the active [`theme`] palette so the dark/light
//! setting actually changes how the UI looks.

use crate::CONFIG;
use crate::argus::focus::FocusManager;
use crate::argus::focus::FocusTarget;
use crate::argus::state::{AppState, LogLevel, RuntimeState, Section};
use crate::argus::theme;
use crate::minecraft::optimization::OptimizationProfile;
use crate::servers::{ServerEntry, ping_server};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph};
use std::sync::Mutex;

/// Hit-test map rebuilt every frame: (rect, target_id) pairs for mouse
/// interaction. Clicking a rect focuses its target; actionable ids activate.
pub static MOUSE_TARGETS: Mutex<Vec<(Rect, String)>> = Mutex::new(Vec::new());

/// Register a screen rect for a focus target during drawing.
pub fn register_hit(id: &str, area: Rect) {
    if let Ok(mut map) = MOUSE_TARGETS.lock() {
        // Later registrations win on overlap; push then resolve by last match.
        map.retain(|(r, existing)| !(r == &area && existing == id));
        map.push((area, id.to_string()));
    }
}

/// Find the topmost registered target whose rect contains the point.
pub fn hit_test(col: u16, row: u16) -> Option<String> {
    let map = MOUSE_TARGETS.lock().ok()?;
    let pos = Position { x: col, y: row };
    map.iter()
        .rev()
        .find(|(r, _)| r.contains(pos))
        .map(|(_, id)| id.clone())
}

/// Style helper for terminal panels — rounded borders, themed background.
fn panel_block(title: String) -> Block<'static> {
    let t = theme::current();
    Block::new()
        .border_type(BorderType::Rounded)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.border))
        .title(title)
        .title_style(Style::default().fg(t.accent_dim).bold())
        .bg(t.bg_panel)
}

/// Draw the top navigation bar (tabs)
pub fn draw_navbar(frame: &mut Frame, area: Rect, state: &AppState, focus: &FocusManager) {
    let t = theme::current();
    let sections = Section::all();

    // Navbar background
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(t.bg_panel)),
        area,
    );

    // Render a continuous border at the bottom of the navbar.
    let border_y = area.bottom();
    frame.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(if state.current_section == Section::Home {
                t.accent
            } else {
                t.border_dim
            }),
        ))
        .style(Style::default().bg(t.bg_panel)),
        Rect::new(area.left(), border_y, area.width, 1),
    );

    let tab_count = sections.len().max(1);
    let constraints: Vec<Constraint> = sections
        .iter()
        .map(|_| Constraint::Percentage(100 / tab_count as u16))
        .collect();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(&constraints[..])
        .split(area);

    for (i, chunk) in chunks.iter().enumerate() {
        let section = sections[i];
        let is_active = section == state.current_section;
        let nav_id = format!("nav_{}", section.label().to_lowercase().replace(' ', "_"));
        let is_focused = focus.current().map(|f| f.id == nav_id).unwrap_or(false);

        let (fg, bg) = if is_active {
            (t.bg, t.accent)
        } else if is_focused {
            (t.accent, t.bg_filled)
        } else {
            (t.text_muted, t.bg_panel)
        };

        let indicator = if is_active { "▶ " } else { "  " };
        let bullet = if is_active { "●" } else { "○" };
        let label = section.label();

        let text = vec![Line::from(vec![
            Span::styled(indicator, Style::default().fg(fg).bg(bg)),
            Span::styled(bullet.to_string(), Style::default().fg(fg).bg(bg).bold()),
            Span::styled(format!(" {} ", label), Style::default().fg(fg).bg(bg).bold()),
            Span::styled(" ".repeat(4), Style::default().fg(t.bg_filled).bg(bg)),
        ])];

        register_hit(&nav_id, *chunk);
        frame.render_widget(
            Paragraph::new(text)
                .block(Block::new().bg(bg))
                .alignment(Alignment::Center),
            *chunk,
        );
    }
}

/// Draw the header with title and runtime status
pub fn draw_header(frame: &mut Frame, area: Rect, state: &AppState, _focus: &FocusManager) {
    let t = theme::current();

    let player_name = state
        .active_account_name
        .as_deref()
        .unwrap_or("Steve");
    let runtime = &state.runtime_state;
    let status_label = runtime.label();
    let java_version = state
        .java_installations
        .first()
        .and_then(|j| j.version.as_ref())
        .map(|v| v.major.to_string())
        .unwrap_or_else(|| "?".to_string());
    // Required inner width: "<icon> <LABEL> Java <major> · Player: <name>"
    let inner_needed = 1 /* icon */
        + 1 + status_label.chars().count()
        + 1 + 5 /* "Java " */
        + java_version.chars().count()
        + 3 /* " · " */
        + 9 /* "Player: " */
        + player_name.chars().count();
    // Outer width = inner + 2 borders. Clamp to [30, 56] so the left pane still has room.
    let status_width = (inner_needed + 2).clamp(30, 56).min(area.width as usize) as u16;

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(status_width)])
        .split(area);

    // Left side: title
    let title = vec![
        Line::from(Span::styled(
            concat!("ERA LAUNCHER v", env!("CARGO_PKG_VERSION")),
            Style::default().fg(t.accent).bold(),
        )),
        Line::from(Span::styled(
            "ARGUS — Minecraft Runtime Control Terminal",
            Style::default().fg(t.text_subtle),
        )),
    ];
    frame.render_widget(
        Paragraph::new(title).block(
            Block::new()
                .border_type(BorderType::Rounded)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border))
                .title(" System ".to_string())
                .title_style(Style::default().fg(t.accent).bold())
                .bg(t.bg_panel),
        ),
        chunks[0],
    );

    // Right side: runtime status
    let status_color = match runtime {
        RuntimeState::Running => t.success,
        RuntimeState::Error(_) => t.error,
        RuntimeState::Starting => t.info,
        RuntimeState::Stopping => t.warning,
        RuntimeState::Stopped => t.text_subtle,
    };

    let status_icon = runtime.status_indicator();

    // Inner width available for the name = status_width - 2 (borders) - everything else on the line.
    let name_budget = status_width
        .saturating_sub(22 + status_label.chars().count() as u16 + java_version.chars().count() as u16)
        .max(1) as usize;
    let player_name = truncate_label(player_name, name_budget);

    let status_text = vec![Line::from(vec![
        Span::styled(format!("{} ", status_icon), Style::default().fg(status_color)),
        Span::styled(format!("{} ", status_label), Style::default().fg(status_color).bold()),
        Span::styled(format!("Java {}", java_version), Style::default().fg(t.text_dim)),
        Span::styled(" · ", Style::default().fg(t.divider)),
        Span::styled(
            format!("Player: {}", player_name),
            Style::default().fg(t.text_dim),
        ),
    ])];

    let header_title = format!(" {} STATUS ", status_label.to_uppercase());
    frame.render_widget(
        Paragraph::new(status_text)
            .block(
                Block::new()
                    .border_type(BorderType::Rounded)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(status_color))
                    .title(header_title)
                    .title_style(Style::default().fg(status_color).bold())
                    .bg(t.bg_panel),
            )
            .alignment(Alignment::Right),
        chunks[1],
    );
}

/// Draw the detail pane for the currently selected instance
pub fn draw_instance_detail(frame: &mut Frame, area: Rect, state: &AppState, focus: &FocusManager) {
    let t = theme::current();

    if let Some(inst) = state.selected_instance.as_ref() {
        let loader_color = loader_color(inst.loader.as_str());
        let version_str = inst.loader_version.as_deref().unwrap_or("default");

        // Detail rows — simplified: show version numbers, not raw paths
        let java_display = inst.java.as_ref()
            .and_then(|p| extract_java_version(p))
            .unwrap_or_else(|| inst.java.as_deref().unwrap_or("Auto-detect").to_string());

        let rows: Vec<(Color, &str, String)> = vec![
            (t.text_dim, "Game",      inst.game_version.clone()),
            (loader_color, "Loader",  format!("{} {}", inst.loader.to_uppercase(), version_str)),
            (t.text_dim, "RAM",       format!("{} GB", inst.memory / 1024)),
            (t.text_dim, "Java",      java_display),
            (t.text_dim, "Resolution", inst.resolution_width.map(|w| format!("{}x{}", w, inst.resolution_height.unwrap_or(0))).unwrap_or_else(|| "Default".to_string())),
        ];

        let mut items = Vec::new();
        for (fg, label, value) in rows {
            items.push(ListItem::new(vec![Line::from(vec![
                Span::styled(format!("{:<12}", label), Style::default().fg(fg).bold()),
                Span::styled(value.clone(), Style::default().fg(t.text)),
            ])]));
            items.push(ListItem::new(vec![Line::from(Span::raw(""))]));
        }

        let detail_list = List::new(items)
            .block(panel_block(" INSTANCE ".to_string()));

        // Play button — at the bottom of the detail pane
        let play_id = "home_play";
        let is_play_focused = focus.current().map(|f| f.id == play_id).unwrap_or(false);

        let btn_h = 3u16;
        let gap = 1u16;
        let list_height = area.height.saturating_sub(btn_h + gap);
        let btn_area = Rect::new(area.left(), area.top() + list_height + gap, area.width, btn_h);
        let list_area = Rect::new(area.left(), area.top(), area.width, list_height);

        // Fill the full area with panel background so the border box is complete
        frame.render_widget(
            Paragraph::new("").style(Style::default().bg(t.bg_panel)),
            area,
        );

        frame.render_widget(detail_list, list_area);

        register_hit(play_id, btn_area);
        let play_border = if is_play_focused { t.info } else { t.border_dim };
        let play_bg = t.bg_panel;
        let play_fg = if is_play_focused { t.text } else { t.text_dim };
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled("▶ PLAY", Style::default().fg(play_fg).bg(play_bg).bold()))])
                .block(
                    Block::new()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(play_border))
                        .bg(play_bg),
                )
                .alignment(Alignment::Center),
            btn_area,
        );
    } else {
        // No selection
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                "Select an instance to view details.",
                Style::default().fg(t.text_dim),
            ))])
            .block(panel_block(" DETAILS ".to_string())),
            area,
        );
    }
}

/// Draw instance list for the INSTANCES section
pub fn draw_instance_list(frame: &mut Frame, area: Rect, state: &AppState, focus: &FocusManager, show_action_bar: bool) {
    let t = theme::current();

    // Fill the inner area with panel background
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(t.bg_panel)),
        area,
    );

    // Split area: list on top, optional action bar at bottom
    let (list_area, action_area) = if show_action_bar {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),        // Instance list
                Constraint::Length(1),     // Spacer
                Constraint::Length(3),     // Action bar
            ])
            .split(area);
        (chunks[0], Some(chunks[2]))
    } else {
        (area, None)
    };

    let mut items = Vec::new();
    let mut focused_idx = 0usize;

    for (i, inst) in state.instances.iter().enumerate() {
        let is_selected = state
            .selected_instance
            .as_ref()
            .map(|s| s.id == inst.id)
            .unwrap_or(false);
        let id = format!("instance_{}", i);
        let is_focused = focus.current().map(|f| f.id == id).unwrap_or(false);
        if is_focused {
            focused_idx = i;
        }

        let loader_color = loader_color(inst.loader.as_str());

        let bg = if is_focused {
            t.selection
        } else {
            t.bg_panel
        };

        let badge = if is_selected {
            Span::styled(" ★ ", Style::default().fg(t.warning).bold().bg(bg))
        } else if is_focused {
            Span::styled(" ▶ ", Style::default().fg(t.accent).bold().bg(bg))
        } else {
            Span::raw("   ")
        };

        let loader_badge = match inst.loader.as_str() {
            "fabric" => format!("[FABRIC v{}]", inst.loader_version.as_deref().unwrap_or("?")),
            "forge" => format!("[FORGE v{}]", inst.loader_version.as_deref().unwrap_or("?")),
            "vanilla" => "[VANILLA]".to_string(),
            _ => format!("[{}]", inst.loader.to_uppercase()),
        };

        let title = Line::from(vec![
            badge,
            Span::styled(
                format!("{} ", inst.name),
                Style::default().fg(if is_focused { t.bg } else { t.text }).bold().bg(bg),
            ),
            Span::styled(
                format!("{} ", loader_badge),
                Style::default().fg(loader_color).bold().bg(bg),
            ),
            Span::styled(
                format!("  MC {}", inst.game_version),
                Style::default().fg(t.text_dim).bg(bg),
            ),
        ]);

        let java_short = inst.java.as_ref()
            .and_then(|p| extract_java_version(p))
            .unwrap_or_else(|| "auto".to_string());

        let detail = Line::from(vec![
            Span::styled("   ", Style::default().bg(bg)),
            Span::styled(
                format!("RAM: {}GB", inst.memory / 1024),
                Style::default().fg(t.text_subtle).bg(bg),
            ),
            Span::styled(format!(" · Java: {}", java_short), Style::default().fg(t.text_subtle).bg(bg)),
            Span::styled(" ".to_string(), Style::default().bg(bg)),
        ]);

        items.push(ListItem::new(vec![title, detail, Line::from(Span::raw(" "))]));
    }

    if items.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "  No instances found.",
            Style::default().fg(t.text_dim),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "  Press 'c' to create one.",
            Style::default().fg(t.accent_dim),
        ))]));
    }

    let list = List::new(items)
        .block(
            if show_action_bar {
                // Outer border is drawn by draw_home; no inner border needed
                Block::new().bg(t.bg_panel)
            } else {
                // Standalone list (e.g. INSTANCES section) — needs its own border+title
                panel_block(format!(" INSTANCES  ({} total)", state.instances.len()))
            }
        )
        .style(Style::default().fg(t.text).bg(t.bg))
        .highlight_style(Style::default().bg(t.selection).fg(t.text));

    // Selection follows the FOCUSED row so ↑↓ scrolls the list.
    let selected = focus
        .current()
        .map(|f| f.id.starts_with("instance_"))
        .unwrap_or(false)
        .then_some(focused_idx)
        .or_else(|| {
            state
                .selected_instance
                .as_ref()
                .and_then(|s| state.instances.iter().position(|i| i.id == s.id))
        })
        .unwrap_or(0);

     register_hit("instances_list", list_area);
    let mut list_state = ListState::default();
    list_state.select(Some(selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    // Action buttons at bottom (only for HOME screen)
    if let Some(ab_area) = action_area {
        let actions: Vec<(String, FocusTarget)> = vec![
            ("[ + New ]".to_string(), FocusTarget::new("home_create", "Create")),
            ("[ Folder ]".to_string(), FocusTarget::new("home_open", "Open")),
        ];
        draw_action_bar(frame, ab_area, &actions, focus);
    }
}

fn loader_color(loader: &str) -> Color {
    let t = theme::current();
    match loader {
        "fabric" => t.fabric,
        "forge" => t.forge,
        "vanilla" => t.vanilla,
        _ => t.text_dim,
    }
}

/// Extract a short Java version string with vendor from a java executable path.
/// e.g. "C:\Java25\jdk-25.0.4.1+1\bin\java.exe" -> "Temurin 25"
fn extract_java_version(path: &str) -> Option<String> {
    use std::path::Path;
    let p = Path::new(path);
    let path_str = p.to_string_lossy().to_lowercase();

    // Detect vendor from known path patterns
    let vendor = if path_str.contains("EraLauncher") || path_str.contains("adoptium") || path_str.contains("temurin") || (path_str.contains("jdk-") && path_str.contains("+")) {
        "Temurin"
    } else if path_str.contains("zulu") {
        "Zulu"
    } else if path_str.contains("corretto") {
        "Corretto"
    } else if path_str.contains("openjdk") || path_str.contains("java-") {
        "OpenJDK"
    } else {
        "Java"
    };

    // Look for a path component that has a version number (e.g. "jdk-25.0.4.1", "Java25", "java-25")
    for component in p.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            // Try patterns like "jdk-25.0.4.1" or "java-25"
            let digits: String = name_str.chars().filter(|c| c.is_ascii_digit()).collect();
            if let Ok(major) = digits.parse::<u32>() {
                if major >= 8 {
                    return Some(format!("{} {}", vendor, major));
                }
            }
            // Pattern like "Java25" where version is embedded
            let embedded: String = name_str.chars().skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(major) = embedded.parse::<u32>() {
                if major >= 8 {
                    return Some(format!("{} {}", vendor, major));
                }
            }
        }
    }
    None
}

/// Draw the HOME screen — two-pane layout:
///   instance list (left) | detail pane (right)
pub fn draw_home(frame: &mut Frame, area: Rect, state: &AppState, focus: &FocusManager) {
    let t = theme::current();

    // Two columns: instance list + detail
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(44), // Instance list
            Constraint::Min(1),     // Detail pane (takes remaining space)
        ])
        .split(area);

    // Fill both columns background
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(t.bg)),
        area,
    );

    // Left column: panel that wraps list + action bar
    // Use full main[0] height; the list expands to fill, action bar at bottom.
    let outer = Block::new()
        .border_type(BorderType::Rounded)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.border))
        .title(" INSTANCES ".to_string())
        .title_style(Style::default().fg(t.accent_dim).bold())
        .bg(t.bg_panel);

    let inner = outer.inner(main[0]);
    frame.render_widget(outer, main[0]);

    // Left: instance list (no own border — uses outer panel)
    draw_instance_list(frame, inner, state, focus, true);

    // Right: detail pane (with its own panel_block border)
    draw_instance_detail(frame, main[1], state, focus);
}

/// Draw compact action buttons in a horizontal bar
pub fn draw_action_bar(
    frame: &mut Frame,
    area: Rect,
    actions: &[(String, FocusTarget)],
    focus: &FocusManager,
) {
    let t = theme::current();

    // Background for the action bar area
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(t.bg_panel)),
        Rect::new(area.left(), area.top(), area.width, area.height),
    );

    let btn_h = 3u16;
    let btn_w = 16u16;
    let btn_gap = 3u16;
    let total_w = (btn_w + btn_gap) * actions.len() as u16 - btn_gap;
    // Center the buttons vertically and horizontally within the area
    let start_y = area.top() + (area.height.saturating_sub(btn_h)) / 2;
    let start_x = area.left() + (area.width.saturating_sub(total_w)) / 2;

     for (i, (label, target)) in actions.iter().enumerate() {
        let is_focused = focus.current().map(|f| f.id == target.id).unwrap_or(false);

        let x = start_x + (i as u16 * (btn_w + btn_gap));

        if is_focused {
            // Blue outline for focused action button — distinct from green Play button
            let rect = Rect::new(x, start_y, btn_w, btn_h);
            register_hit(&target.id, rect);
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(
                    label.chars().take((btn_w - 4) as usize).collect::<String>(),
                    Style::default().fg(t.text).bg(t.bg_panel).bold(),
                ))])
                .block(
                    Block::new()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(t.info))
                        .bg(t.bg_panel)
                        .border_type(BorderType::Rounded),
                )
                .alignment(Alignment::Center),
                rect,
            );
        } else {
            let bg = t.bg_panel;
            let fg = t.text_dim;
            let rect = Rect::new(x, start_y, btn_w, btn_h);
            let block = Block::new()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border_dim))
                .bg(bg)
                .border_type(BorderType::Rounded);

            let label_text = label.chars().take((btn_w - 4) as usize).collect::<String>();
            register_hit(&target.id, rect);
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(
                    label_text,
                    Style::default().fg(fg).bg(bg).bold(),
                ))])
                .block(block)
                .alignment(Alignment::Center),
                rect,
            );
        }
    }
}

/// Draw the DISCOVER screen — categories live INSIDE this view as a
/// navigable left-hand selector; no separate tabs.
pub fn draw_discover(frame: &mut Frame, area: Rect, state: &AppState, focus: &FocusManager) {
    use crate::argus::state::DiscoverTab;

    let t = theme::current();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Search bar
            Constraint::Min(0),    // Category selector + results
        ])
        .split(area);

    // --- Search bar ---
    let search_active = state.search_mode;
    let search_border = if search_active {
        t.border_focus
    } else if focus
        .current()
        .map(|f| f.id == "disc_search")
        .unwrap_or(false)
    {
        t.focus
    } else {
        t.border
    };
    let hint = if search_active {
        "type query · ENTER search · ESC cancel"
    } else {
        "press / or ENTER on this field to type"
    };
    let cursor = if search_active { "▉" } else { "" };
    let search_text = vec![Line::from(vec![
        Span::styled("🔍 ", Style::default().fg(t.text_dim)),
        Span::styled(state.discover_search.clone(), Style::default().fg(t.text)),
        Span::styled(cursor, Style::default().fg(t.accent)),
        Span::styled(format!("  ({})", hint), Style::default().fg(t.text_muted)),
    ])];
    frame.render_widget(
        Paragraph::new(search_text).block(
            Block::new()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(search_border))
                .title("Search Modrinth".to_string())
                .title_style(Style::default().fg(t.accent_dim).bold())
                .bg(t.bg_panel),
        ),
        outer[0],
    );

    // --- Categories (left) + Results (right) ---
    let sidebar_width = (area.width / 4).clamp(18, 32);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(0)])
        .split(outer[1]);

    // Left: category selector INSIDE Discover
    let tabs = DiscoverTab::all();
    let mut cat_items = Vec::new();
    cat_items.push(ListItem::new(vec![Line::from(Span::styled(
        "CATEGORY",
        Style::default().fg(t.accent_dim).bold(),
    ))]));
    for (i, tab) in tabs.iter().enumerate() {
        let is_active = state.discover_tab == *tab;
        let id = format!("disc_cat_{}", i);
        let is_focused = focus.current().map(|f| f.id == id).unwrap_or(false);
        let style = if is_focused || is_active {
            Style::default()
                .fg(if is_active { t.bg } else { t.focus })
                .bg(if is_active { t.accent } else { t.bg })
                .bold()
        } else {
            Style::default().fg(t.text_dim).bg(t.bg)
        };
        let bullet = if is_active { "●" } else { "○" };
        cat_items.push(ListItem::new(vec![Line::from(vec![
            Span::styled(format!("[{}] ", i + 1), style),
            Span::styled(format!("{} ", bullet), style),
            Span::styled(tab.label().to_string(), style),
        ])]));
    }
    cat_items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    cat_items.push(ListItem::new(vec![Line::from(Span::styled(
        "[1-4] quick switch",
        Style::default().fg(t.text_muted),
    ))]));

    let cat_list = List::new(cat_items).block(
        Block::new()
            .borders(Borders::ALL)
            .title(
                if state.discover_pane == crate::argus::state::DiscoverPane::Categories {
                    "DISCOVER · pick category (ESC → tabs)".to_string()
                } else {
                    "DISCOVER · categories".to_string()
                },
            )
            .title_style(Style::default().fg(t.accent_dim).bold())
            .border_style(Style::default().fg(
                if state.discover_pane == crate::argus::state::DiscoverPane::Categories {
                    t.border_focus
                } else {
                    t.border
                },
            ))
            .bg(t.bg_panel),
    );
    register_hit("discover_categories", columns[0]);
    frame.render_widget(cat_list, columns[0]);

    // Right: results rendered statefully so highlight + scroll follow focus.
    // Installed projects are hidden unless the `i` reveal toggle is on;
    // revealed ones carry a ✓ badge and stay fully manageable.
    let visible = state.visible_project_indices();
    let mut items = Vec::new();
    let mut focused_row = 0usize;
    for (row, i) in visible.iter().enumerate() {
        let project = &state.modrinth_results[*i];
        let id = format!("project_{}", i);
        let is_focused = focus.current().map(|f| f.id == id).unwrap_or(false);
        if is_focused {
            focused_row = row;
        }
        let is_installed = state.result_installed.get(*i).copied().unwrap_or(false);
        let bg = if is_focused { t.bg_dark } else { t.bg };

        let title_line = Line::from(vec![
            Span::styled(
                format!("[{}] ", row + 1),
                Style::default().fg(t.accent).bg(bg),
            ),
            Span::styled(
                format!("{} ", project.title),
                Style::default().fg(t.text).bold().bg(bg),
            ),
            Span::styled(
                format!(" ⬇ {}", format_downloads(project.downloads)),
                Style::default().fg(t.text_dim).bg(bg),
            ),
            Span::styled(
                format!("  {}", project.author),
                Style::default().fg(t.text_muted).bg(bg),
            ),
            if is_installed {
                Span::styled(
                    "  ✓ installed",
                    Style::default().fg(t.success).bg(bg).bold(),
                )
            } else {
                Span::raw("")
            },
        ]);
        let desc_line = Line::from(Span::styled(
            format!(
                "     {}",
                project
                    .description
                    .as_str()
                    .chars()
                    .take(70)
                    .collect::<String>()
            ),
            Style::default().fg(t.text_dim).bg(bg),
        ));
        items.push(ListItem::new(vec![title_line, desc_line]));
    }

    if items.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            format!(
                "No {} loaded yet — wait for results or press / to search.",
                state.discover_tab.label().to_lowercase()
            ),
            Style::default().fg(t.text_dim),
        ))]));
    }

    let list = List::new(items)
        .block(
            Block::new()
                .borders(Borders::ALL)
                .title(format!(
                    "{} RESULTS  (MC {} · {} shown · {} installed hidden · [i] reveal)",
                    state.discover_tab.label().to_uppercase(),
                    if state.discover_game_version.is_empty() {
                        "?"
                    } else {
                        &state.discover_game_version
                    },
                    visible.len(),
                    state.discover_hidden_count
                ))
                .title_style(Style::default().fg(t.accent_dim).bold())
                .border_style(Style::default().fg(
                    if state.discover_pane == crate::argus::state::DiscoverPane::Results {
                        t.border_focus
                    } else {
                        t.border
                    },
                ))
                .bg(t.bg_panel),
        )
        .style(Style::default().fg(t.text).bg(t.bg))
        .highlight_style(Style::default().bg(t.bg_dark).fg(t.accent))
        .highlight_symbol("▸ ");
    register_hit("discover_results", columns[1]);
    let mut list_state = ListState::default();
    list_state.select(Some(focused_row));
    frame.render_stateful_widget(list, columns[1], &mut list_state);
}

/// Human-readable download counts (12345 → "12.3k").
fn format_downloads(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Draw the LOGS screen (scrollable — newest at top, PgUp/PgDn/wheel scroll)
pub fn draw_logs(frame: &mut Frame, area: Rect, state: &AppState, _focus: &FocusManager) {
    let t = theme::current();
    let mut items = Vec::new();

    if state.live_log_view {
        // Live tail of the argus-live.log capture file written while the
        // game is running. We display the most recent 500 lines.
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "▸ LIVE — Shift+L to switch back to in-memory logs",
            Style::default().fg(t.accent_dim).bold(),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
        if let Some(path) = &state.live_log_path {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    for line in content.lines().rev().take(500) {
                        items.push(ListItem::new(vec![Line::from(Span::styled(
                            line.to_string(),
                            Style::default().fg(t.text),
                        ))]));
                    }
                }
                Err(_) => {
                    items.push(ListItem::new(vec![Line::from(Span::styled(
                        "(no live log file yet — launch a game to start capturing)",
                        Style::default().fg(t.text_dim),
                    ))]));
                }
            }
        } else {
            items.push(ListItem::new(vec![Line::from(Span::styled(
                "No instance selected.",
                Style::default().fg(t.text_dim),
            ))]));
        }
    } else {
        // Skip the newest `log_scroll` entries so scrolling moves back in time.
        for entry in state.logs.iter().rev().skip(state.log_scroll).take(500) {
            let color = match entry.level {
                LogLevel::Error => t.error,
                LogLevel::Warn => t.warning,
                LogLevel::Info => t.info,
                LogLevel::Debug => t.text_muted,
            };
            let line = Line::from(vec![
                Span::styled(
                    format!("[{}]", entry.timestamp),
                    Style::default().fg(t.text_muted),
                ),
                Span::styled(" ", Style::default()),
                Span::styled(
                    format!("[{}]", entry.level.label()),
                    Style::default().fg(color),
                ),
                Span::styled(" ", Style::default()),
                Span::styled(
                    format!("<{}>", entry.source),
                    Style::default().fg(t.text_muted),
                ),
                Span::styled(" ", Style::default()),
                Span::styled(entry.message.as_str(), Style::default().fg(t.text)),
            ]);
            items.push(ListItem::new(vec![line]));
        }
    }

    if items.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "No log entries yet.",
            Style::default().fg(t.text_dim),
        ))]));
    }

    let list = List::new(items).block(panel_block(format!(
        "LOGS  ({} entries · wheel/PgUp-PgDn scroll)",
        state.logs.len()
    )));
    register_hit("logs_list", area);
    frame.render_widget(list, area);
}

/// Draw the SETTINGS screen
pub fn draw_settings(frame: &mut Frame, area: Rect, state: &AppState, focus: &FocusManager) {
    use crate::argus::state::SettingsEditMode;

    let t = theme::current();
    // Snapshot the values we need and RELEASE the lock immediately: the
    // selector overlay below re-reads CONFIG, and holding this guard across
    // that call deadlocked the same thread (std Mutex is not re-entrant) —
    // ENTER on "Default Memory" froze the launcher permanently.
    let (
        default_memory,
        java_path,
        theme_name,
        language,
        optimization_profile,
        win_w,
        win_h,
        win_max,
    ) = {
        let config = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        (
            config.settings.default_memory,
            config.settings.java_path.clone(),
            config.settings.theme.clone(),
            config.settings.language.clone(),
            config.settings.optimization_profile,
            config.window.width,
            config.window.height,
            config.window.maximized,
        )
    };
    let in_edit = state.settings_edit_mode != SettingsEditMode::None;

    // If in edit mode, draw the selector overlay instead (guard released).
    if in_edit {
        draw_settings_selector(frame, area, state, focus);
        return;
    }

    let mut items = Vec::new();

    // Helper to check if a setting is currently focused
    let focused_style = |id: &str| -> Style {
        if focus.current().map(|f| f.id == id).unwrap_or(false) {
            Style::default().fg(t.bg).bg(t.accent).bold()
        } else {
            Style::default().fg(t.text)
        }
    };

    let memory_style = focused_style("settings_memory");
    let java_style = focused_style("settings_java");
    let theme_style = focused_style("settings_theme");
    let language_style = focused_style("settings_language");
    let optimization_style = focused_style("settings_optimization");
    let window_style = focused_style("settings_window");
    let account_style = focused_style("settings_account");

    let account_label = state
        .active_account_name
        .clone()
        .unwrap_or_else(|| "Default (Steve)".to_string());

    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("◯ ", Style::default().fg(t.border)),
        Span::styled("Default Memory: ", Style::default().fg(t.text_dim)),
        Span::styled(format!("{} MB", default_memory), memory_style),
        Span::styled("  (ENTER to change)", Style::default().fg(t.text_muted)),
    ])]));
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("◯ ", Style::default().fg(t.border)),
        Span::styled("Java Path: ", Style::default().fg(t.text_dim)),
        Span::styled(java_path.as_deref().unwrap_or("Auto-detect"), java_style),
        Span::styled("  (ENTER to change)", Style::default().fg(t.text_muted)),
    ])]));
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("◯ ", Style::default().fg(t.border)),
        Span::styled("Theme: ", Style::default().fg(t.text_dim)),
        Span::styled(theme_name, theme_style),
        Span::styled("  (ENTER to change)", Style::default().fg(t.text_muted)),
    ])]));
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("◯ ", Style::default().fg(t.border)),
        Span::styled("Language: ", Style::default().fg(t.text_dim)),
        Span::styled(language, language_style),
        Span::styled("  (ENTER to change)", Style::default().fg(t.text_muted)),
    ])]));
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("◯ ", Style::default().fg(t.border)),
        Span::styled("Optimization: ", Style::default().fg(t.text_dim)),
        Span::styled(optimization_profile.as_str(), optimization_style),
        Span::styled("  (ENTER to change)", Style::default().fg(t.text_muted)),
    ])]));
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("◯ ", Style::default().fg(t.border)),
        Span::styled("Offline Account: ", Style::default().fg(t.text_dim)),
        Span::styled(account_label, account_style),
        Span::styled("  (ENTER to manage)", Style::default().fg(t.text_muted)),
    ])]));
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("◯ ", Style::default().fg(t.border)),
        Span::styled("Window: ", Style::default().fg(t.text_dim)),
        Span::styled(
            format!("{}x{} (maximized: {})", win_w, win_h, win_max),
            window_style,
        ),
        Span::raw(" "),
    ])]));
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "Detected Java Installations:",
        Style::default().fg(t.accent_dim).bold(),
    ))]));
    for (i, j) in state.java_installations.iter().enumerate() {
        let java_id = format!("java_{}", i);
        let is_java_focused = focus.current().map(|f| f.id == java_id).unwrap_or(false);
        let java_dot_style = if is_java_focused {
            Style::default().fg(t.accent).bold()
        } else {
            Style::default().fg(t.text_dim)
        };
        let version_str = j
            .version
            .as_ref()
            .map(|v| format!("Java {}", v.major))
            .unwrap_or_else(|| "Unknown".to_string());
        items.push(ListItem::new(vec![Line::from(vec![
            Span::styled("  ◯ ", java_dot_style),
            Span::styled(
                format!("{}  ", version_str),
                if is_java_focused {
                    Style::default().fg(t.bg).bg(t.accent).bold()
                } else {
                    Style::default().fg(t.text)
                },
            ),
            Span::styled(
                j.path.to_string_lossy().to_string(),
                Style::default().fg(t.text_muted),
            ),
        ])]));
    }

    let list = List::new(items).block(panel_block(" SETTINGS ".to_string()));
    frame.render_widget(list, area);
}

/// Draw the settings selector overlay (memory picker, Java picker, theme picker)
fn draw_settings_selector(frame: &mut Frame, area: Rect, state: &AppState, _focus: &FocusManager) {
    use crate::argus::state::{MEMORY_PRESETS, SettingsEditMode};

    let t = theme::current();
    let (title, items): (String, Vec<String>) = match state.settings_edit_mode {
        SettingsEditMode::MemorySelector => {
            let presets = MEMORY_PRESETS;
            let selected = state.settings_edit_index;
            let current = {
                let config = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
                config.settings.default_memory
            };
            let mut items = Vec::new();
            for (i, &mb) in presets.iter().enumerate() {
                let label = if i == selected {
                    format!("← {} MB  selected", mb)
                } else {
                    format!("  {} MB", mb)
                };
                if mb == current && i != selected {
                    items.push(format!("  {} MB  (current)", mb));
                } else {
                    items.push(label);
                }
            }
            ("DEFAULT MEMORY".to_string(), items)
        }
        SettingsEditMode::JavaSelector => {
            let mut items = Vec::new();
            items.push(
                if state.settings_edit_index == 0 {
                    "← Auto-detect  selected".to_string()
                } else {
                    "  Auto-detect".to_string()
                }
            );
            for (i, j) in state.java_installations.iter().enumerate() {
                let version_str = j
                    .version
                    .as_ref()
                    .map(|v| format!("Java {}", v.major))
                    .unwrap_or_else(|| "Unknown".to_string());
                let path_str = j.path.to_string_lossy();
                let label = if i + 1 == state.settings_edit_index {
                    format!("← {}  {}", version_str, path_str)
                } else {
                    format!("  {}  {}", version_str, path_str)
                };
                items.push(label);
            }
            ("JAVA PATH".to_string(), items)
        }
        SettingsEditMode::ThemeSelector => {
            let themes = &state.theme_options;
            let mut items = Vec::new();
            for (i, thm) in themes.iter().enumerate() {
                let label = if i == state.settings_edit_index {
                    format!("← {}  selected", thm)
                } else {
                    format!("  {}", thm)
                };
                items.push(label);
            }
            ("THEME".to_string(), items)
        }
        SettingsEditMode::LanguageInfo => {
            let items = vec![
                "  en".to_string(),
                "  [Only English currently available]".to_string(),
            ];
            ("LANGUAGE".to_string(), items)
        }
        SettingsEditMode::OptimizationSelector => {
            let profiles = OptimizationProfile::all();
            let mut items = Vec::new();
            for (i, profile) in profiles.iter().enumerate() {
                let label = if i == state.settings_edit_index {
                    format!("← {}  selected", profile.as_str())
                } else {
                    format!("  {}", profile.as_str())
                };
                items.push(label);
            }
            ("OPTIMIZATION PROFILE".to_string(), items)
        }
        SettingsEditMode::CustomJvmEditor => {
            let cfg = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
            let current = cfg.settings.custom_jvm_args.join(" ");
            drop(cfg);
            let input = if state.custom_jvm_input.is_empty() {
                current.clone()
            } else {
                state.custom_jvm_input.clone()
            };
            let title = "CUSTOM JVM ARGS".to_string();
            let hint = "Edit args separated by spaces. ENTER to save, ESC to cancel.".to_string();
            let items = vec![
                hint,
                format!(
                    "Current: {}",
                    if current.is_empty() {
                        "(none)".to_string()
                    } else {
                        current.clone()
                    }
                ),
                format!("> {}", input),
            ];
            (title, items)
        }
        SettingsEditMode::None => return,
    };

    let items_iter = items.iter().map(|s| {
        ListItem::new(vec![Line::from(Span::styled(
            s,
            Style::default().fg(t.text),
        ))])
    });

    let list = List::new(items_iter).block(
        Block::new()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(t.border_focus))
            .title_style(Style::default().fg(t.accent).bold()),
    );

    frame.render_widget(list, area);
}

/// Short display tag for an installed-content kind.
fn short_kind(kind: &str) -> &'static str {
    match kind {
        "MOD" => "Mods",
        "RESOURCE PACK" => "Rpacks",
        "SHADER" => "Shaders",
        "MODPACK" => "Mpacks",
        _ => "Other",
    }
}

/// Draw the MODS section — shows content ACTUALLY installed in the selected
/// instance (mods, resource packs, shaders) by scanning its directories.
pub fn draw_mods(frame: &mut Frame, area: Rect, state: &AppState, focus: &FocusManager) {
    let t = theme::current();
    let mut items = Vec::new();
    // Header occupies the first two list entries so selection math must
    // account for them when mirroring keyboard focus.
    let mut selected_row = 0usize;

    let target = state
        .selected_instance
        .as_ref()
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "(no instance)".to_string());

    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("Installed content for ", Style::default().fg(t.text_dim)),
        Span::styled(target, Style::default().fg(t.accent).bold()),
    ])]));
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));

    for (i, item) in state.installed_content.iter().enumerate() {
        let id = format!("installed_{}", i);
        let row = i + 2;
        let is_focused = focus.current().map(|f| f.id == id).unwrap_or(false);
        if is_focused {
            selected_row = row;
        }
        let bg = if is_focused { t.bg_dark } else { t.bg };
        let kind_color = match item.kind {
            "MOD" => t.fabric,
            "RESOURCE PACK" => t.warning,
            "SHADER" => t.info,
            _ => t.text_dim,
        };
        items.push(ListItem::new(vec![Line::from(vec![
            Span::styled(
                format!("[ {:<7}] ", short_kind(item.kind)),
                Style::default().fg(kind_color).bold().bg(bg),
            ),
            Span::styled(item.name.clone(), Style::default().fg(t.text).bg(bg)),
            Span::styled(
                format!("  ({})", format_size(item.size_bytes)),
                Style::default().fg(t.text_muted).bg(bg),
            ),
            if is_focused {
                Span::styled("  ← X removes", Style::default().fg(t.error).bg(bg).bold())
            } else {
                Span::raw("")
            },
        ])]));
    }

    if state.installed_content.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "Nothing installed yet.",
            Style::default().fg(t.text_dim),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "Open DISCOVER (d), pick a category and press ENTER on a project to install it.",
            Style::default().fg(t.text_muted),
        ))]));
    }

    if !state.updatable_mods.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "Updates available:",
            Style::default().fg(t.warning).bold(),
        ))]));
        for (i, u) in state.updatable_mods.iter().enumerate() {
            let id = format!("update_{}", i);
            let row = items.len();
            let is_focused = focus.current().map(|f| f.id == id).unwrap_or(false);
            if is_focused {
                selected_row = row;
            }
            let bg = if is_focused { t.bg_dark } else { t.bg };
            items.push(ListItem::new(vec![Line::from(vec![
                Span::styled(
                    "[ UPDATE ] ".to_string(),
                    Style::default().fg(t.warning).bold().bg(bg),
                ),
                Span::styled(u.title.clone(), Style::default().fg(t.text).bg(bg)),
                Span::styled(
                    format!("  {} → {}", u.installed_version, u.latest_version),
                    Style::default().fg(t.text_muted).bg(bg),
                ),
            ])]));
        }
    }

    let list = List::new(items)
        .block(panel_block(format!(
            "INSTALLED CONTENT  ({} file(s) · ↑↓ browse · X removes focused)",
            state.installed_content.len()
        )))
        .style(Style::default().fg(t.text).bg(t.bg))
        .highlight_style(Style::default().bg(t.bg_dark))
        .highlight_symbol("▸ ");

    // Stateful render keeps the focused row visible (auto-scroll).
    register_hit("mods_list", area);
    let mut ls = ListState::default();
    ls.select(Some(selected_row));
    frame.render_stateful_widget(list, area, &mut ls);
}

/// Format a byte size for display.
fn format_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Draw the WORLDS section — lists worlds found in the selected instance's
/// saves directory.
pub fn draw_worlds(frame: &mut Frame, area: Rect, state: &AppState, _focus: &FocusManager) {
    let t = theme::current();
    let mut items = Vec::new();

    let target = state
        .selected_instance
        .as_ref()
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "(no instance)".to_string());
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("Worlds for ", Style::default().fg(t.text_dim)),
        Span::styled(target, Style::default().fg(t.accent).bold()),
    ])]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "  b — Backup ·  r — Restore ·  O — Open folder ·  d — Delete",
        Style::default().fg(t.text_muted),
    ))]));
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));

    for world in &state.worlds {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            format!("🌍 {}", world),
            Style::default().fg(t.text),
        ))]));
    }

    if state.worlds.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "No worlds found.",
            Style::default().fg(t.text_dim),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "Worlds will appear here once Minecraft has been launched.",
            Style::default().fg(t.text_muted),
        ))]));
    }

    let list = List::new(items).block(panel_block("WORLDS".to_string()));
    frame.render_widget(list, area);
}

/// Draw the CRASHES section — lists JVM crash reports found for the selected
/// instance.
pub fn draw_crashes(frame: &mut Frame, area: Rect, state: &AppState, _focus: &FocusManager) {
    let t = theme::current();
    let mut items = Vec::new();

    let target = state
        .selected_instance
        .as_ref()
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "(no instance)".to_string());
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("Crash reports for ", Style::default().fg(t.text_dim)),
        Span::styled(target, Style::default().fg(t.accent).bold()),
    ])]));
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "  c — Copy ·  d — Delete ·  o — Open folder",
        Style::default().fg(t.text_muted),
    ))]));
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));

    for report in &state.crash_reports {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            format!("{} — {}", report.timestamp, report.summary),
            Style::default().fg(t.error),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::styled(
            format!("  Exception: {}", report.exception),
            Style::default().fg(t.text_muted),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::styled(
            format!("  Thread: {}", report.thread),
            Style::default().fg(t.text_muted),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::styled(
            format!("  JVM: {}", report.jvm_version),
            Style::default().fg(t.text_muted),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    }

    if !state.crash_diagnostics.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "Diagnostic hints:",
            Style::default().fg(t.accent).bold(),
        ))]));
        for hint in &state.crash_diagnostics {
            items.push(ListItem::new(vec![Line::from(vec![
                Span::styled("→ ", Style::default().fg(t.accent_dim).bold()),
                Span::styled(hint.label(), Style::default().fg(t.info)),
            ])]));
        }
        items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    }

    if state.crash_reports.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "No crash reports found.",
            Style::default().fg(t.text_dim),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "Reports appear here after Minecraft exits abnormally.",
            Style::default().fg(t.text_muted),
        ))]));
    }

    let list = List::new(items).block(panel_block("CRASH REPORTS".to_string()));
    frame.render_widget(list, area);
}

/// Draw the SERVERS section — favourite multiplayer servers with quick
/// ping (press ENTER to ping the focused row).
pub fn draw_servers(frame: &mut Frame, area: Rect, state: &AppState, _focus: &FocusManager) {
    let t = theme::current();
    let mut items: Vec<ListItem> = Vec::new();

    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled(
            "Press ENTER to ping ·  a  to add a new server ·  r  to refresh the list",
            Style::default().fg(t.text_muted),
        ),
    ])]));
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));

    if state.servers.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "No servers yet — press 'a' to add one (e.g. hypixel.net:25565).",
            Style::default().fg(t.text_dim),
        ))]));
    } else {
        for (_i, server) in state.servers.iter().enumerate() {
            let ping_line = match state.server_pings.get(&server.id) {
                Some(p) => format!(
                    "{} · {}/{} · {} ms · {}",
                    truncate_label(&p.description, 28),
                    p.players_online,
                    p.players_max,
                    p.latency_ms,
                    p.version_name
                ),
                None => "—".to_string(),
            };
            let ping_color = match state.server_pings.get(&server.id) {
                Some(p) if p.latency_ms < 80 => t.success,
                Some(_) => t.warning,
                None => t.text_muted,
            };
            items.push(ListItem::new(vec![Line::from(vec![
                Span::styled(
                    format!("▸ {:<22}", truncate_label(&server.name, 22)),
                    Style::default().fg(t.accent).bold(),
                ),
                Span::styled(
                    format!(" {:<22}", truncate_label(&server.address, 22)),
                    Style::default().fg(t.text_dim),
                ),
                Span::styled(ping_line, Style::default().fg(ping_color)),
            ])]));
        }
    }
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "a — Add server",
        Style::default().fg(t.text_dim),
    ))]));

    let list = List::new(items).block(panel_block("SERVERS".to_string()));
    frame.render_widget(list, area);
}

/// Draw the SCREENSHOTS section — vanilla `screenshots/` directory listing.
pub fn draw_screenshots(frame: &mut Frame, area: Rect, state: &AppState, _focus: &FocusManager) {
    let t = theme::current();
    let mut items: Vec<ListItem> = Vec::new();

    let target = state
        .selected_instance
        .as_ref()
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "(no instance)".to_string());
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("Screenshots for ", Style::default().fg(t.text_dim)),
        Span::styled(target, Style::default().fg(t.accent).bold()),
    ])]));
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));

    if state.screenshots.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "No screenshots yet.",
            Style::default().fg(t.text_dim),
        ))]));
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "Press F2 in Minecraft to capture one. They land in <instance>/game/screenshots/.",
            Style::default().fg(t.text_muted),
        ))]));
    } else {
        for entry in &state.screenshots {
            items.push(ListItem::new(vec![Line::from(vec![
                Span::styled("▸ ", Style::default().fg(t.accent_dim)),
                Span::styled(
                    format!("{:<32}", truncate_label(&entry.path.file_name().unwrap_or_default().to_string_lossy(), 32)),
                    Style::default().fg(t.text),
                ),
                Span::styled(
                    format!(" {:<16}  ", entry.mtime),
                    Style::default().fg(t.text_dim),
                ),
                Span::styled(
                    format!("{}", human_bytes(entry.size_bytes)),
                    Style::default().fg(t.text_muted),
                ),
            ])]));
        }
    }
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "ENTER — Open ·  o  — Open folder",
        Style::default().fg(t.text_dim),
    ))]));

    let list = List::new(items).block(panel_block("SCREENSHOTS".to_string()));
    frame.render_widget(list, area);
}

fn truncate_label(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", head)
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

/// Load favourite servers from disk into the state. Safe to call on every
/// entry into the SERVERS section (cheap).
pub fn refresh_servers(state: &mut AppState) {
    state.servers = crate::servers::list_servers();
}

/// Load screenshots for the currently-selected instance.
pub fn refresh_screenshots(state: &mut AppState) {
    use crate::argus::state::ScreenshotEntry;
    let Some(inst) = state.selected_instance.clone() else {
        state.screenshots.clear();
        return;
    };
    let dir = crate::argus::backend::BackendBridge::instances_dir()
        .join(inst.id)
        .join("game")
        .join("screenshots");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| {
                    if d.as_secs() < 86400 {
                        format!("{}h ago", d.as_secs() / 3600)
                    } else {
                        format!("{}d ago", d.as_secs() / 86400)
                    }
                })
                .unwrap_or_else(|| "?".to_string());
            out.push(ScreenshotEntry {
                path,
                mtime,
                size_bytes: meta.len(),
            });
        }
    }
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    state.screenshots = out;
}

// Re-export so app.rs can call into the module without a separate import.
pub use crate::servers::ServerEntry as _ServerEntryReexport;

// Silence unused-import warnings for items only used by tests.
#[allow(dead_code)]
fn _keep_imports() {
    let _ = ServerEntry::new;
    let _: fn(&str) -> Result<crate::servers::PingInfo, crate::errors::LauncherError> = ping_server;
}

/// Draw the command prompt
pub fn draw_command_prompt(frame: &mut Frame, area: Rect, state: &AppState) {
    let t = theme::current();
    let prompt_style = Style::default().fg(t.accent).bold();
    let input_style = Style::default().fg(t.text);

    let lines = vec![Line::from(vec![
        Span::styled("> ", prompt_style),
        Span::styled(&state.command_input, input_style),
        Span::raw("▉"),
    ])];

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::new()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border_focus))
                .title("COMMAND".to_string())
                .bg(t.bg_dark),
        ),
        area,
    );
}

/// Draw the status bar at the bottom
pub fn draw_status_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    use crate::argus::update::UpdateCheckResult;
    let t = theme::current();
    let update_note = match &state.update_check {
        UpdateCheckResult::UpdateAvailable(tag) => {
            format!("  ⬆ Update available: {} — type 'update'", tag)
        }
        UpdateCheckResult::CheckFailed(err) => {
            format!("  ⚠ Update check failed: {}", err)
        }
        UpdateCheckResult::UpToDate => String::new(),
    };
    let text = if state.command_prompt_active {
        " [COMMAND] Type command and press ENTER. ESC to cancel.".to_string()
    } else if state.search_mode {
        " [SEARCH] Type a Modrinth query — ENTER to search, ESC to cancel.".to_string()
    } else if state.loading {
        format!(
            " [LOADING] {} ",
            state.loading_message.as_deref().unwrap_or("...")
        )
    } else if let Some(ref err) = state.error_message {
        format!(" [ERROR] {} — ESC dismisses ", err)
    } else if let Some((msg, instant)) = &state.status_message {
        let elapsed = instant.elapsed();
        if elapsed.as_secs() < 5 {
            format!(" [INFO] {} ", msg)
        } else {
            let section_label = state.current_section.label();
            let theme_name = t.name;
            format!(
                " {} | {} theme | [←→] sections  [↑↓] items  [TAB] navbar  [ENTER] activate  [/] search  [?] help{}",
                section_label, theme_name, update_note
            )
        }
    } else {
        let section_label = state.current_section.label();
        let theme_name = t.name;
        format!(
            " {} | {} theme | [←→] sections  [↑↓] items  [TAB] navbar  [ENTER] activate  [/] search  [?] help{}",
            section_label, theme_name, update_note
        )
    };

    let bg = if state.error_message.is_some() {
        t.error
    } else if state.command_prompt_active || state.search_mode {
        t.accent
    } else {
        t.bg_dark
    };

    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            text,
            Style::default().fg(if state.error_message.is_some() { t.bg } else { t.text_dim }).bg(bg).bold(),
        ))])
        .bg(bg),
        area,
    );
}

/// Draw a loading progress bar with an animated braille spinner
pub fn draw_loading_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    let t = theme::current();
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let spin = SPINNER[(state.tick as usize) % SPINNER.len()];
    let bar_width = ((area.width as f32) * 0.5) as u32;
    let bar = "█".repeat(bar_width as usize);
    let empty = "░".repeat((area.width as usize).saturating_sub(bar.len()));

    let text = format!(
        " {} {} {} {}",
        spin,
        bar,
        empty,
        state.loading_message.as_deref().unwrap_or("Loading..."),
    );

    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            text,
            Style::default().fg(t.accent).bg(t.bg_dark),
        ))])
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border))
                .bg(t.bg_dark),
        )
        .bg(t.bg_dark),
        area,
    );
}

/// Draw the create-instance loader picker overlay
pub fn draw_loader_selector(frame: &mut Frame, area: Rect, state: &AppState) {
    use crate::argus::state::CREATE_LOADERS;

    let t = theme::current();
    let width = area.width.min(58);
    let height = (CREATE_LOADERS.len() as u16 + 5).min(area.height);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let modal = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(ratatui::widgets::Clear, modal);

    let mut items = vec![ListItem::new(vec![Line::from(Span::styled(
        "CHOOSE MOD LOADER",
        Style::default().fg(t.accent).bold(),
    ))])];
    for (i, (id, desc)) in CREATE_LOADERS.iter().enumerate() {
        let selected = i == state.loader_selector_index;
        let marker = if selected { "▸" } else { " " };
        let style = if selected {
            Style::default().fg(t.bg).bg(t.accent).bold()
        } else {
            Style::default().fg(t.text)
        };
        items.push(ListItem::new(vec![Line::from(vec![
            Span::styled(format!(" {} [{}] ", marker, i + 1), style),
            Span::styled(format!("{:<8}", id.to_uppercase()), style),
            Span::styled(desc.to_string(), style),
        ])]));
    }
    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "↑↓ choose · ENTER create · ESC cancel",
        Style::default().fg(t.text_muted),
    ))]));

    let list = List::new(items).block(
        Block::new()
            .borders(Borders::ALL)
            .title("CREATE INSTANCE".to_string())
            .title_style(Style::default().fg(t.accent).bold())
            .border_style(Style::default().fg(t.border_focus))
            .bg(t.bg_panel),
    );
    frame.render_widget(list, modal);
}

/// Draw the game-version picker overlay (create flow step 2)
pub fn draw_version_selector(frame: &mut Frame, area: Rect, state: &AppState) {
    let t = theme::current();
    let loader = state
        .pending_version_loader
        .as_deref()
        .unwrap_or("vanilla")
        .to_uppercase();

    // Recompute the filtered list identically to the handler.
    let needle = state.version_filter.to_lowercase();
    let list: Vec<String> = state
        .versions
        .iter()
        .filter(|v| needle.is_empty() || v.to_lowercase().contains(&needle))
        .cloned()
        .collect();

    let width = area.width.min(46);
    let height = area.height.min(22);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let modal = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, modal);

    let vis_rows = (modal.height as usize).saturating_sub(6).max(1);
    let idx = state
        .version_selector_index
        .min(list.len().saturating_sub(1));
    let mut start = idx.saturating_sub(vis_rows.saturating_sub(1));
    if idx >= start + vis_rows {
        start = idx + 1 - vis_rows;
    }
    let end = (start + vis_rows).min(list.len());

    let mut items = Vec::new();
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled(
            "SELECT GAME VERSION  ",
            Style::default().fg(t.accent).bold(),
        ),
        Span::styled(loader, Style::default().fg(t.focus).bold()),
    ])]));
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("filter> ", Style::default().fg(t.text_muted)),
        Span::styled(state.version_filter.clone(), Style::default().fg(t.text)),
        Span::styled("▉", Style::default().fg(t.accent)),
        Span::styled(
            format!("  {} match(es)", list.len()),
            Style::default().fg(t.text_muted),
        ),
    ])]));

    for (i, version) in list[start..end].iter().enumerate() {
        let real_idx = start + i;
        let selected = real_idx == idx;
        let marker = if selected { "▸" } else { " " };
        let style = if selected {
            Style::default().fg(t.bg).bg(t.accent).bold()
        } else {
            Style::default().fg(t.text)
        };
        items.push(ListItem::new(vec![Line::from(Span::styled(
            format!(" {} {}", marker, version),
            style,
        ))]));
    }
    if list.is_empty() {
        items.push(ListItem::new(vec![Line::from(Span::styled(
            "  no versions match",
            Style::default().fg(t.error),
        ))]));
    }

    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "type to filter · ↑↓/PgUp-Dn · ENTER create",
        Style::default().fg(t.text_muted),
    ))]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "ESC back to loader choice",
        Style::default().fg(t.text_muted),
    ))]));

    frame.render_widget(
        List::new(items).block(
            Block::new()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border_focus))
                .bg(t.bg_panel),
        ),
        modal,
    );
}

/// Shorten a label to `max` visible chars with an ellipsis.
/// Draw the per-mod install version chooser overlay
pub fn draw_install_version_overlay(frame: &mut Frame, area: Rect, state: &AppState) {
    let t = theme::current();
    let Some(pi) = state.pending_install.as_ref() else {
        return;
    };

    let width = area.width.min(64);
    let height = area.height.min(22);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let modal = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, modal);

    let vis_rows = (modal.height as usize).saturating_sub(6).max(1);
    let idx = state
        .install_version_index
        .min(pi.rows.len().saturating_sub(1));
    let mut start = idx.saturating_sub(vis_rows.saturating_sub(1));
    if idx >= start + vis_rows {
        start = idx + 1 - vis_rows;
    }
    let end = (start + vis_rows).min(pi.rows.len());

    let mut items = Vec::new();
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled("CHOOSE VERSION  ", Style::default().fg(t.accent).bold()),
        Span::styled(
            truncate_label(&pi.title, 34),
            Style::default().fg(t.text).bold(),
        ),
    ])]));
    items.push(ListItem::new(vec![Line::from(vec![
        Span::styled(
            format!("{} · ", pi.content_type),
            Style::default().fg(t.text_muted),
        ),
        Span::styled(
            "releases listed before alphas",
            Style::default().fg(t.text_muted),
        ),
    ])]));

    for (i, (_vid, label)) in pi.rows[start..end].iter().enumerate() {
        let real_idx = start + i;
        let selected = real_idx == idx;
        let marker = if selected { "▸" } else { " " };
        let style = if selected {
            Style::default().fg(t.bg).bg(t.accent).bold()
        } else {
            Style::default().fg(t.text)
        };
        items.push(ListItem::new(vec![Line::from(Span::styled(
            format!(" {} {}", marker, label),
            style,
        ))]));
    }

    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "↑↓/PgUp-Dn pick build · ENTER installs it · ESC cancel",
        Style::default().fg(t.text_muted),
    ))]));

    frame.render_widget(
        List::new(items).block(
            Block::new()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border_focus))
                .bg(t.bg_panel),
        ),
        modal,
    );
}

/// Draw the offline-account picker overlay
pub fn draw_account_selector(frame: &mut Frame, area: Rect, state: &AppState) {
    let t = theme::current();
    let rows = state.accounts.len() + 3; // header + accounts + create + hints
    let width = area.width.min(56);
    let height = (rows as u16 + 4).min(area.height);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let modal = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(ratatui::widgets::Clear, modal);

    let mut items = vec![ListItem::new(vec![Line::from(Span::styled(
        "ACCOUNTS",
        Style::default().fg(t.accent).bold(),
    ))])];

    for (i, (_id, name)) in state.accounts.iter().enumerate() {
        let selected = i == state.account_selector_index;
        let is_active = state.active_account_name.as_deref() == Some(name.as_str());
        let marker = if selected { "▸" } else { " " };
        let dot = if is_active { "●" } else { "○" };
        let style = if selected {
            Style::default().fg(t.bg).bg(t.accent).bold()
        } else if is_active {
            Style::default().fg(t.accent)
        } else {
            Style::default().fg(t.text)
        };
        items.push(ListItem::new(vec![Line::from(vec![
            Span::styled(format!(" {} ", marker), style),
            Span::styled(format!("{} ", dot), style),
            Span::styled(name.clone(), style),
            if is_active {
                Span::styled("  (launch account)", style)
            } else {
                Span::raw("")
            },
        ])]));
    }

    // Create-new row
    let create_idx = state.accounts.len();
    let create_selected = create_idx == state.account_selector_index;
    let create_style = if create_selected {
        Style::default().fg(t.bg).bg(t.accent).bold()
    } else {
        Style::default().fg(t.focus)
    };
    items.push(ListItem::new(vec![Line::from(Span::styled(
        format!(
            " {} [+ Create offline account ]",
            if create_selected { "▸" } else { " " }
        ),
        create_style,
    ))]));

    items.push(ListItem::new(vec![Line::from(Span::raw(" "))]));
    items.push(ListItem::new(vec![Line::from(Span::styled(
        "↑↓ choose · ENTER select · N new · X delete · ESC",
        Style::default().fg(t.text_muted),
    ))]));

    frame.render_widget(
        List::new(items).block(
            Block::new()
                .borders(Borders::ALL)
                .title("ACCOUNTS".to_string())
                .title_style(Style::default().fg(t.accent).bold())
                .border_style(Style::default().fg(t.border_focus))
                .bg(t.bg_panel),
        ),
        modal,
    );
}

/// Draw the new-account name input modal
pub fn draw_account_input(frame: &mut Frame, area: Rect, state: &AppState) {
    use ratatui::widgets::Clear;
    let t = theme::current();
    let width = area.width.min(52);
    let height = 7.min(area.height);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let modal = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, modal);

    let cursor = "▉";
    let lines = vec![
        Line::from(Span::styled(
            "NEW OFFLINE ACCOUNT",
            Style::default().fg(t.accent).bold(),
        )),
        Line::from(Span::raw(" ")),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(t.accent)),
            Span::styled(state.account_input.clone(), Style::default().fg(t.text)),
            Span::styled(cursor, Style::default().fg(t.accent)),
        ]),
        Line::from(Span::raw(" ")),
        Line::from(Span::styled(
            "3-16 chars · letters/numbers/underscore",
            Style::default().fg(t.text_muted),
        )),
        Line::from(Span::styled(
            "ENTER create & select · ESC cancel",
            Style::default().fg(t.text_muted),
        )),
    ];

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::new()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border_focus))
                .bg(t.bg_panel),
        ),
        modal,
    );
}

/// Draw the keyboard-shortcuts help overlay (toggled with ?)
pub fn draw_help_overlay(frame: &mut Frame, area: Rect, _state: &AppState) {
    let t = theme::current();
    let width = area.width.min(72);
    let height = area.height.min(26);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let modal = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(ratatui::widgets::Clear, modal);

    let rows: [(&str, &str); 19] = [
        ("← / →", "Switch section — lands INSIDE its content"),
        ("↑ / ↓", "Move within section items (wraps, never leaves)"),
        ("TAB / SHIFT+TAB", "Reach the navbar / cycle every control"),
        ("ENTER", "Activate focused control"),
        ("ENTER (navbar)", "Jump into that section's items"),
        ("ENTER (instance)", "Select — again to launch"),
        ("ENTER (result)", "Choose a build & install"),
        ("1 – 4", "Discover: open category & jump into results"),
        ("I", "Discover: reveal/hide installed projects"),
        ("/ or F", "Search Modrinth"),
        ("C", "Create instance (HOME): loader → version"),
        (
            "X",
            "INSTANCES: delete instance · MODS: remove focused file",
        ),
        ("CTRL+L", "Open command prompt"),
        ("SETTINGS → Account", "ENTER manages offline accounts"),
        ("ESC (results)", "Back to DISCOVER categories"),
        ("ESC (categories)", "Back to the main tabs"),
        ("PGUP / PGDN", "Fast-step items / scroll LOGS"),
        ("?", "Toggle this help"),
        ("Q", "Quit ARGUS"),
    ];
    let mut items: Vec<Line> = vec![Line::from("")];
    for (key, desc) in &rows {
        items.push(Line::from(vec![
            Span::styled(
                format!(" {:>14} ", key),
                Style::default().fg(t.bg).bg(t.accent).bold(),
            ),
            Span::styled(format!("{}  ", desc), Style::default().fg(t.text)),
        ]));
    }

    frame.render_widget(
        Paragraph::new(items).block(
            Block::new()
                .borders(Borders::ALL)
                .title("KEYBOARD SHORTCUTS  (? or ESC to close)".to_string())
                .title_style(Style::default().fg(t.accent).bold())
                .border_style(Style::default().fg(t.border_focus))
                .bg(t.bg_panel),
        ),
        modal,
    );
}
