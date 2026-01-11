//! Terminal UI dashboard using ratatui.
//!
//! Provides a btop-style dashboard for managing neuraphage tasks.

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Sparkline, Wrap};

use crate::error::Result;
use crate::task::{Task, TaskStatus};
use crate::ui::notifications::Notification;

/// Application state for the TUI.
#[derive(Debug, Clone)]
pub struct AppState {
    /// List of tasks.
    pub tasks: Vec<Task>,
    /// Currently selected task index.
    pub selected_task: usize,
    /// Recent notifications.
    pub notifications: Vec<Notification>,
    /// API request rate history (for sparkline).
    pub request_rates: Vec<u64>,
    /// Total cost in USD.
    pub total_cost: f64,
    /// Total tokens used.
    pub total_tokens: u64,
    /// Running task count.
    pub running_count: usize,
    /// Waiting task count.
    pub waiting_count: usize,
    /// Queued task count.
    pub queued_count: usize,
    /// Whether to show help.
    pub show_help: bool,
    /// Current conversation output for selected task.
    pub conversation_output: Vec<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            selected_task: 0,
            notifications: Vec::new(),
            request_rates: Vec::new(),
            total_cost: 0.0,
            total_tokens: 0,
            running_count: 0,
            waiting_count: 0,
            queued_count: 0,
            show_help: false,
            conversation_output: Vec::new(),
        }
    }
}

impl AppState {
    /// Create new app state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update task counts from task list.
    pub fn update_counts(&mut self) {
        self.running_count = self.tasks.iter().filter(|t| t.status == TaskStatus::Running).count();
        self.waiting_count = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::WaitingForUser)
            .count();
        self.queued_count = self.tasks.iter().filter(|t| t.status == TaskStatus::Queued).count();
    }

    /// Navigate task selection down.
    pub fn next_task(&mut self) {
        if !self.tasks.is_empty() {
            self.selected_task = (self.selected_task + 1) % self.tasks.len();
        }
    }

    /// Navigate task selection up.
    pub fn prev_task(&mut self) {
        if !self.tasks.is_empty() {
            self.selected_task = self.selected_task.checked_sub(1).unwrap_or(self.tasks.len() - 1);
        }
    }

    /// Get the currently selected task.
    pub fn selected(&self) -> Option<&Task> {
        self.tasks.get(self.selected_task)
    }
}

/// TUI application.
pub struct App {
    /// Application state.
    pub state: AppState,
    /// Whether the app should quit.
    pub should_quit: bool,
}

impl App {
    /// Create a new TUI app.
    pub fn new() -> Self {
        Self {
            state: AppState::new(),
            should_quit: false,
        }
    }

    /// Handle input events.
    pub fn handle_event(&mut self, event: Event) {
        if let Event::Key(key) = event {
            self.handle_key(key);
        }
    }

    /// Handle key press.
    fn handle_key(&mut self, key: KeyEvent) {
        // Handle Ctrl+C
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.state.next_task(),
            KeyCode::Char('k') | KeyCode::Up => self.state.prev_task(),
            KeyCode::Char('?') => self.state.show_help = !self.state.show_help,
            KeyCode::Esc => self.state.show_help = false,
            _ => {}
        }
    }

    /// Render the UI.
    pub fn render(&self, frame: &mut Frame) {
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(10),   // Main content
                Constraint::Length(5), // Notifications
                Constraint::Length(1), // Status bar
            ])
            .split(frame.area());

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(main_layout[0]);

        let left_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(8)])
            .split(content_layout[0]);

        // Render each section
        self.render_task_list(frame, left_layout[0]);
        self.render_stats(frame, left_layout[1]);
        self.render_task_detail(frame, content_layout[1]);
        self.render_notifications(frame, main_layout[1]);
        self.render_status_bar(frame, main_layout[2]);

        // Render help overlay if shown
        if self.state.show_help {
            self.render_help_popup(frame);
        }
    }

    /// Render the task list.
    fn render_task_list(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .state
            .tasks
            .iter()
            .map(|task| {
                let icon = match task.status {
                    TaskStatus::Running => "●".green(),
                    TaskStatus::Queued => "○".white(),
                    TaskStatus::WaitingForUser => "?".yellow(),
                    TaskStatus::Blocked => "◌".red(),
                    TaskStatus::Paused => "◑".yellow(),
                    TaskStatus::Completed => "✓".green(),
                    TaskStatus::Failed => "✗".red(),
                    TaskStatus::Cancelled => "⊘".white(),
                };

                let status_str = match task.status {
                    TaskStatus::Running => "Running".green(),
                    TaskStatus::Queued => "Queued".white(),
                    TaskStatus::WaitingForUser => "Waiting".yellow(),
                    TaskStatus::Blocked => "Blocked".red(),
                    TaskStatus::Paused => "Paused".yellow(),
                    TaskStatus::Completed => "Done".green(),
                    TaskStatus::Failed => "Failed".red(),
                    TaskStatus::Cancelled => "Cancelled".white(),
                };

                let short_id = &task.id.0[..task.id.0.len().min(8)];
                let short_desc = if task.description.len() > 20 {
                    format!("{}...", &task.description[..17])
                } else {
                    task.description.clone()
                };

                let line = Line::from(vec![
                    icon,
                    " ".into(),
                    short_id.cyan(),
                    " ".into(),
                    short_desc.into(),
                    " ".into(),
                    status_str,
                ]);

                ListItem::new(line)
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(self.state.selected_task));

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Tasks ")
                    .borders(Borders::ALL)
                    .border_set(border::ROUNDED),
            )
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

        frame.render_stateful_widget(list, area, &mut list_state);
    }

    /// Render the stats panel.
    fn render_stats(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Stats ")
            .borders(Borders::ALL)
            .border_set(border::ROUNDED);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(inner);

        // Task counts
        let counts = format!(
            "Running: {}  Waiting: {}  Queued: {}",
            self.state.running_count, self.state.waiting_count, self.state.queued_count
        );
        frame.render_widget(Paragraph::new(counts), layout[0]);

        // Cost and tokens
        let usage = format!(
            "Cost: ${:.2}  Tokens: {:.1}k",
            self.state.total_cost,
            self.state.total_tokens as f64 / 1000.0
        );
        frame.render_widget(Paragraph::new(usage), layout[1]);

        // API rate sparkline
        if !self.state.request_rates.is_empty() {
            let sparkline = Sparkline::default()
                .data(&self.state.request_rates)
                .style(Style::default().fg(Color::Cyan));

            let rate_line = Line::from(vec!["API: ".into(), Span::raw("")]);
            frame.render_widget(Paragraph::new(rate_line), layout[2]);

            if layout.len() > 3 {
                frame.render_widget(sparkline, layout[3]);
            }
        }
    }

    /// Render the task detail panel.
    fn render_task_detail(&self, frame: &mut Frame, area: Rect) {
        let title = if let Some(task) = self.state.selected() {
            let short_desc = if task.description.len() > 40 {
                format!("{}...", &task.description[..37])
            } else {
                task.description.clone()
            };
            format!(" Task: {} ", short_desc)
        } else {
            " No task selected ".to_string()
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_set(border::ROUNDED);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.state.conversation_output.is_empty() {
            let placeholder = Paragraph::new("Select a task to view conversation...")
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: true });
            frame.render_widget(placeholder, inner);
        } else {
            let text: Vec<Line> = self
                .state
                .conversation_output
                .iter()
                .map(|s| Line::from(s.as_str()))
                .collect();
            let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
            frame.render_widget(paragraph, inner);
        }

        // Progress bar for running tasks
        if let Some(task) = self.state.selected()
            && task.status == TaskStatus::Running
        {
            let progress_area = Rect {
                x: inner.x,
                y: inner.y + inner.height.saturating_sub(2),
                width: inner.width,
                height: 1,
            };

            let progress = task.iteration as f64 / 100.0; // Placeholder
            let gauge = Gauge::default()
                .ratio(progress.min(1.0))
                .gauge_style(Style::default().fg(Color::Green))
                .use_unicode(true);
            frame.render_widget(gauge, progress_area);
        }
    }

    /// Render the notifications panel.
    fn render_notifications(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Notifications ")
            .borders(Borders::ALL)
            .border_set(border::ROUNDED);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let items: Vec<ListItem> = self
            .state
            .notifications
            .iter()
            .take(inner.height as usize)
            .map(|n| {
                let time = n.timestamp.format("%H:%M").to_string();
                let style = if n.read { Style::default().fg(Color::DarkGray) } else { Style::default() };
                let line = Line::from(vec![time.dark_gray(), " ".into(), n.message.clone().into()]).style(style);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items);
        frame.render_widget(list, inner);
    }

    /// Render the status bar.
    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        let help_text = " [j/k] Navigate  [Enter] Attach  [n] New  [d] Detach  [q] Quit  [?] Help ";
        let style = Style::default().bg(Color::DarkGray).fg(Color::White);
        let paragraph = Paragraph::new(help_text).style(style);
        frame.render_widget(paragraph, area);
    }

    /// Render help popup overlay.
    fn render_help_popup(&self, frame: &mut Frame) {
        let area = centered_rect(60, 70, frame.area());

        // Clear the area behind the popup
        frame.render_widget(Clear, area);

        let help_text = vec![
            Line::from("Keyboard Shortcuts".bold()),
            Line::from(""),
            Line::from(vec!["j/↓".cyan(), " - Next task".into()]),
            Line::from(vec!["k/↑".cyan(), " - Previous task".into()]),
            Line::from(vec!["Enter".cyan(), " - Attach to task".into()]),
            Line::from(vec!["n".cyan(), " - Create new task".into()]),
            Line::from(vec!["d".cyan(), " - Detach from task".into()]),
            Line::from(vec!["p".cyan(), " - Pause/resume task".into()]),
            Line::from(vec!["x".cyan(), " - Cancel task".into()]),
            Line::from(vec!["r".cyan(), " - Refresh".into()]),
            Line::from(vec!["?".cyan(), " - Toggle help".into()]),
            Line::from(vec!["q".cyan(), " - Quit".into()]),
            Line::from(vec!["Ctrl+C".cyan(), " - Force quit".into()]),
            Line::from(""),
            Line::from("Press ESC or ? to close".dark_gray()),
        ];

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .style(Style::default().bg(Color::Black));

        let paragraph = Paragraph::new(help_text).block(block).wrap(Wrap { trim: true });

        frame.render_widget(paragraph, area);
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper function to create a centered rect.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Terminal wrapper for setup/teardown.
pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Tui {
    /// Initialize the terminal.
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    /// Draw the UI.
    pub fn draw(&mut self, app: &App) -> Result<()> {
        self.terminal.draw(|frame| app.render(frame))?;
        Ok(())
    }

    /// Poll for events with timeout.
    pub fn poll_event(&self, timeout: Duration) -> Result<Option<Event>> {
        if event::poll(timeout)? { Ok(Some(event::read()?)) } else { Ok(None) }
    }

    /// Restore the terminal.
    pub fn restore(&mut self) -> Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_task(id: &str, status: TaskStatus) -> Task {
        Task {
            id: crate::task::TaskId(id.to_string()),
            description: format!("Task {}", id),
            context: None,
            status,
            priority: 2,
            tags: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            closed_at: None,
            close_reason: None,
            parent_id: None,
            iteration: 0,
            tokens_used: 0,
            cost: 0.0,
        }
    }

    #[test]
    fn test_app_state_default() {
        let state = AppState::default();
        assert!(state.tasks.is_empty());
        assert_eq!(state.selected_task, 0);
        assert!(!state.show_help);
    }

    #[test]
    fn test_app_state_navigation() {
        let mut state = AppState {
            tasks: vec![
                test_task("1", TaskStatus::Running),
                test_task("2", TaskStatus::Queued),
                test_task("3", TaskStatus::Completed),
            ],
            ..Default::default()
        };

        assert_eq!(state.selected_task, 0);

        state.next_task();
        assert_eq!(state.selected_task, 1);

        state.next_task();
        assert_eq!(state.selected_task, 2);

        state.next_task(); // Wraps around
        assert_eq!(state.selected_task, 0);

        state.prev_task(); // Wraps around
        assert_eq!(state.selected_task, 2);
    }

    #[test]
    fn test_app_state_update_counts() {
        let mut state = AppState {
            tasks: vec![
                test_task("1", TaskStatus::Running),
                test_task("2", TaskStatus::Running),
                test_task("3", TaskStatus::Queued),
                test_task("4", TaskStatus::WaitingForUser),
                test_task("5", TaskStatus::Completed),
            ],
            ..Default::default()
        };

        state.update_counts();

        assert_eq!(state.running_count, 2);
        assert_eq!(state.queued_count, 1);
        assert_eq!(state.waiting_count, 1);
    }

    #[test]
    fn test_app_handle_quit() {
        let mut app = App::new();
        assert!(!app.should_quit);

        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(app.should_quit);
    }

    #[test]
    fn test_app_handle_navigation() {
        let mut app = App::new();
        app.state.tasks = vec![test_task("1", TaskStatus::Running), test_task("2", TaskStatus::Queued)];

        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)));
        assert_eq!(app.state.selected_task, 1);

        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)));
        assert_eq!(app.state.selected_task, 0);
    }

    #[test]
    fn test_app_handle_help() {
        let mut app = App::new();
        assert!(!app.state.show_help);

        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)));
        assert!(app.state.show_help);

        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.state.show_help);
    }

    #[test]
    fn test_centered_rect() {
        let area = Rect::new(0, 0, 100, 50);
        let centered = centered_rect(50, 50, area);

        // Should be roughly centered
        assert!(centered.x > 0);
        assert!(centered.y > 0);
        assert!(centered.width < area.width);
        assert!(centered.height < area.height);
    }

    #[test]
    fn test_selected_task() {
        let mut state = AppState::default();
        assert!(state.selected().is_none());

        state.tasks = vec![test_task("1", TaskStatus::Running)];
        assert!(state.selected().is_some());
        assert_eq!(state.selected().unwrap().id.0, "1");
    }
}
