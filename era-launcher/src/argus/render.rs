//! Renderer — handles terminal rendering for ARGUS.
//!
//! Uses crossterm for terminal I/O and ratatui for drawing.
//! Manages terminal setup, teardown, and the main render loop.

use crate::argus::Section;
use crate::argus::focus::FocusManager;
use crate::argus::state::AppState;
use crate::argus::ui;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event as CEvent};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::Duration;

static RESIZING: AtomicBool = AtomicBool::new(false);
static LAST_SET_COLS: AtomicU16 = AtomicU16::new(0);
static LAST_SET_ROWS: AtomicU16 = AtomicU16::new(0);

/// On Windows, sync the console buffer size to the current window size.
///
/// crossterm's `SetSize` resizes the window rect but then restores the buffer
/// to its prior size, never actually growing or shrinking it to match. We fix
/// this by calling `SetConsoleScreenBufferSize` directly after `SetSize`
/// has already resized the window via crossterm.
///
/// Retries up to 3 times with a small delay because `ScreenBuffer::current()`
/// can fail on the first call after entering alternate screen (common on
/// fresh launch after auto-update from older versions).
#[cfg(windows)]
fn set_buffer_size(cols: u16, rows: u16) {
    use crossterm::ExecutableCommand;
    use crossterm::terminal::SetSize;
    use crossterm_winapi::ScreenBuffer;
    use std::io::Write;
    use std::thread;
    use std::time::Duration;

    if RESIZING.swap(true, Ordering::SeqCst) {
        return;
    }

    let mut stdout = std::io::stdout();

    let _ = stdout.execute(SetSize(cols, rows));
    let _ = stdout.flush();

    // Retry a few times because ScreenBuffer::current() can fail on the
    // first call after entering alternate screen, especially after an
    // auto-update from an older version that didn't manage the buffer.
    for attempt in 0..3 {
        if let Ok(screen_buffer) = ScreenBuffer::current() {
            let _ = screen_buffer.set_size(cols as i16, rows as i16);
            break;
        }
        if attempt < 2 {
            thread::sleep(Duration::from_millis(50));
        }
    }

    LAST_SET_COLS.store(cols, Ordering::SeqCst);
    LAST_SET_ROWS.store(rows, Ordering::SeqCst);

    RESIZING.store(false, Ordering::SeqCst);
}

/// Non-Windows: no-op (these platforms don't have the console buffer issue).
#[cfg(not(windows))]
fn set_buffer_size(_cols: u16, _rows: u16) {}

/// On Windows, disable the maximize button on the console window to prevent
/// scrollbar artifacts from appearing after maximize/restore cycles.
/// If the window is already maximized, restore it to normal size first.
/// Only removes the maximize box — the window remains resizable.
#[cfg(windows)]
fn disable_maximize() {
    use winapi::um::wincon::GetConsoleWindow;
    use winapi::um::winuser::{
        GWL_STYLE, HWND_TOP, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    use winapi::um::winuser::{IsZoomed, ShowWindow, SW_RESTORE};

    let hwnd = unsafe { GetConsoleWindow() };
    if hwnd.is_null() {
        return;
    }

    // If already maximized, restore to normal so the buffer sync works.
    if unsafe { IsZoomed(hwnd) } != 0 {
        unsafe { ShowWindow(hwnd, SW_RESTORE) };
    }

    let style = unsafe { winapi::um::winuser::GetWindowLongA(hwnd, GWL_STYLE) };
    if style == 0 {
        return;
    }

    // Remove WS_MAXIMIZEBOX (0x00010000) to disable the maximize button.
    // Keep WS_THICKFRAME so the window border style doesn't change, avoiding
    // a white border flash on init.
    let new_style = style & !(0x00010000u32) as i32;

    unsafe {
        winapi::um::winuser::SetWindowLongA(hwnd, GWL_STYLE, new_style);
        winapi::um::winuser::SetWindowPos(
            hwnd,
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
        );
    }
}

/// Non-Windows: no-op.
#[cfg(not(windows))]
fn disable_maximize() {}

/// The renderer handles terminal I/O and the render loop.
pub struct Renderer {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl Renderer {
    /// Initialize the terminal for ARGUS
    pub fn init() -> io::Result<Self> {
        // On Windows, disable maximize and window resizing BEFORE entering
        // the alternate screen to prevent a white border flash.
        disable_maximize();

        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        terminal::enable_raw_mode()?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        // Set buffer size to match the window to prevent scrollbars.
        if let Ok((cols, rows)) = terminal::size() {
            set_buffer_size(cols, rows);
        }

        Ok(Self { terminal })
    }

    /// Handle terminal resize — sync the Windows console buffer to the current
    /// window size to prevent scrollbars from lingering after maximize/restore.
    ///
    /// Uses the size from the `Event::Resize` event (which reflects the window
    /// size Windows just set) rather than calling `terminal::size()` again,
    /// to avoid a stale read if the console hasn't fully updated its internal
    /// state yet.
    pub fn on_resize(&mut self, cols: u16, rows: u16) {
        set_buffer_size(cols, rows);
    }

    /// Clean up terminal on exit
    pub fn deinit(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;
        Ok(())
    }

    /// Get the terminal size as Rect
    pub fn size(&self) -> Rect {
        let size = self
            .terminal
            .size()
            .unwrap_or(ratatui::layout::Size::new(80, 24));
        Rect::new(0, 0, size.width, size.height)
    }

    /// Check whether a resize event is spurious (generated by our own
    /// set_buffer_size call) and should be ignored.
    pub fn is_spurious_resize(cols: u16, rows: u16) -> bool {
        let last_cols = LAST_SET_COLS.load(Ordering::SeqCst);
        let last_rows = LAST_SET_ROWS.load(Ordering::SeqCst);
        last_cols == cols && last_rows == rows
    }

    /// Block on a crossterm event with timeout
    pub fn read_event(&mut self, timeout: Duration) -> Option<CEvent> {
        if event::poll(timeout).unwrap_or(false) {
            event::read().ok()
        } else {
            None
        }
    }

    /// Check if a resize event is available without blocking
    pub fn poll_resize(&mut self) -> Option<(u16, u16)> {
        let timeout = Duration::from_millis(0);
        if event::poll(timeout).unwrap_or(false) {
            if let Ok(event) = event::read() {
                if let CEvent::Resize(cols, rows) = event {
                    return Some((cols, rows));
                }
            }
        }
        None
    }

    /// Render the full ARGUS UI
    pub fn render(&mut self, state: &AppState, focus: &FocusManager) -> io::Result<()> {
        // Resolve the active palette when the settings value changes (the
        // apply() cache makes this a no-op most frames).
        crate::argus::theme::apply(&crate::argus::backend::BackendBridge::get_settings().theme);
        self.terminal.draw(|f| Self::draw_app(f, state, focus))?;
        Ok(())
    }

    /// The main rendering function — draws all UI sections
    fn draw_app(f: &mut Frame, state: &AppState, focus: &FocusManager) {
        // Rebuild the mouse hit-test map for this frame.
        if let Ok(mut map) = ui::MOUSE_TARGETS.lock() {
            map.clear();
        }

        let area = f.area();
        f.render_widget(
            Paragraph::new("").style(Style::default().bg(crate::argus::theme::current().bg)),
            area,
        );

        // Layout: Header (5 lines), Header divider (1), Navbar (3 lines), Content divider (1), Main content, Command/Loading (3 lines), Status (1 line)
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),  // Header
                Constraint::Length(1),  // Header divider
                Constraint::Length(3),  // Navbar
                Constraint::Length(1),  // Content divider
                Constraint::Min(0),     // Main content
                Constraint::Length(3),  // Command/Loading
                Constraint::Length(1), // Status bar
            ])
            .split(area);

        let t = crate::argus::theme::current();

        // Header
        ui::draw_header(f, chunks[0], state, focus);

        // Header divider
        f.render_widget(
            Paragraph::new(Span::styled("─".repeat(area.width as usize), Style::default().fg(t.border))).style(Style::default().bg(t.bg)),
            chunks[1],
        );

        // Navbar (tabs)
        ui::draw_navbar(f, chunks[2], state, focus);

        // Content divider
        f.render_widget(
            Paragraph::new(Span::styled("─".repeat(area.width as usize), Style::default().fg(t.border))).style(Style::default().bg(t.bg)),
            chunks[3],
        );

        // Main content (based on current section)
        match state.current_section {
            Section::Home => ui::draw_home(f, chunks[4], state, focus),
            Section::Discover => ui::draw_discover(f, chunks[4], state, focus),
            Section::Instances => ui::draw_instance_list(f, chunks[4], state, focus, false),
            Section::Mods => ui::draw_mods(f, chunks[4], state, focus),
            Section::Worlds => ui::draw_worlds(f, chunks[4], state, focus),
            Section::Servers => ui::draw_servers(f, chunks[4], state, focus),
            Section::Logs => ui::draw_logs(f, chunks[4], state, focus),
            Section::Crashes => ui::draw_crashes(f, chunks[4], state, focus),
            Section::Screenshots => ui::draw_screenshots(f, chunks[4], state, focus),
            Section::Settings => ui::draw_settings(f, chunks[4], state, focus),
        }

        // Command prompt or loading (or background fill when idle)
        if state.command_prompt_active {
            ui::draw_command_prompt(f, chunks[5], state);
        } else if state.loading {
            ui::draw_loading_bar(f, chunks[5], state);
        } else {
            f.render_widget(
                Paragraph::new("").style(Style::default().bg(crate::argus::theme::current().bg_panel)),
                chunks[5],
            );
        }

        // Status bar
        ui::draw_status_bar(f, chunks[6], state);

        // Help overlay on top of everything
        if state.help_overlay {
            ui::draw_help_overlay(f, area, state);
        }

        // Loader picker overlays the help overlay when open
        if state.loader_selector_open {
            ui::draw_loader_selector(f, area, state);
        }

        // Version picker (create flow step 2)
        if state.version_selector_open {
            ui::draw_version_selector(f, area, state);
        }

        // Per-mod install version chooser on top of results
        if state.pending_install.is_some() {
            ui::draw_install_version_overlay(f, area, state);
        }

        // Account picker / new-account input on top of everything
        if state.account_selector_open {
            ui::draw_account_selector(f, area, state);
        }
        if state.account_input_mode {
            ui::draw_account_input(f, area, state);
        }
    }
}
