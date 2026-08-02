use std::io::Stdout;
use std::io::stdout;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use crossterm::cursor::Show;
use crossterm::event;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use crossterm::execute;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use factory_coordinator::DurableJob;
use factory_coordinator::JobState;
use factory_coordinator::OperationState;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::ListState;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

use crate::transcript::EventTone;
use crate::transcript::Transcript;

pub(crate) enum LiveAction {
    Continue,
    Detach,
}

pub(crate) struct LiveScreen {
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
    selected: Option<usize>,
    follow_tail: bool,
    details_open: bool,
    detail_scroll: u16,
    list_inner: Rect,
    visible_offset: usize,
}

impl LiveScreen {
    pub(crate) fn new() -> Result<Self> {
        enable_raw_mode().context("enable terminal raw mode")?;
        let mut output = stdout();
        if let Err(error) = execute!(output, EnterAlternateScreen, EnableMouseCapture) {
            let _ = execute!(output, DisableMouseCapture, LeaveAlternateScreen, Show);
            let _ = disable_raw_mode();
            return Err(error).context("open Factory live view");
        }
        let terminal = match Terminal::new(CrosstermBackend::new(output)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let mut output = stdout();
                let _ = execute!(output, DisableMouseCapture, LeaveAlternateScreen, Show);
                let _ = disable_raw_mode();
                return Err(error).context("initialize Factory live view");
            }
        };
        Ok(Self {
            terminal: Some(terminal),
            selected: None,
            follow_tail: true,
            details_open: false,
            detail_scroll: 0,
            list_inner: Rect::default(),
            visible_offset: 0,
        })
    }

    pub(crate) fn draw(
        &mut self,
        job: &DurableJob,
        transcript: &Transcript,
        completed: bool,
    ) -> Result<()> {
        self.sync_selection(transcript.rows().len());
        let selected = self.selected;
        let details_open = self.details_open && selected.is_some();
        let detail_scroll = self.detail_scroll;
        let mut list_inner = Rect::default();
        let mut visible_offset = self.visible_offset;
        let terminal = self
            .terminal
            .as_mut()
            .expect("live terminal exists until restore");

        terminal
            .draw(|frame| {
                let constraints = if details_open {
                    vec![
                        Constraint::Length(4),
                        Constraint::Percentage(36),
                        Constraint::Min(7),
                        Constraint::Length(1),
                    ]
                } else {
                    vec![
                        Constraint::Length(4),
                        Constraint::Min(5),
                        Constraint::Length(1),
                    ]
                };
                let areas = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints(constraints)
                    .split(frame.area());

                render_header(frame, areas[0], job);

                let event_area = areas[1];
                let block = Block::default().borders(Borders::ALL).title(format!(
                    " Activity · {} compact cells ",
                    transcript.rows().len()
                ));
                list_inner = block.inner(event_area);
                let preview_width = usize::from(list_inner.width.saturating_sub(20)).max(24);
                let items = transcript
                    .rows()
                    .iter()
                    .map(|row| {
                        let tone = tone_style(row.tone());
                        let stage = row
                            .stage()
                            .map(|stage| format!("{stage:<9} "))
                            .unwrap_or_else(|| "          ".to_string());
                        ListItem::new(Line::from(vec![
                            Span::styled(tone_symbol(row.tone()), tone),
                            Span::raw(" "),
                            Span::styled(stage, Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                format!("{:<9} ", row.label()),
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::raw(row.preview(preview_width)),
                        ]))
                    })
                    .collect::<Vec<_>>();
                let list = List::new(items)
                    .block(block)
                    .highlight_symbol("› ")
                    .highlight_style(Style::default().bg(Color::Rgb(38, 43, 54)));
                let mut state = ListState::default().with_selected(selected);
                *state.offset_mut() = visible_offset;
                frame.render_stateful_widget(list, event_area, &mut state);
                visible_offset = state.offset();

                if details_open {
                    let detail_area = areas[2];
                    let row = &transcript.rows()[selected.expect("checked above")];
                    let title = match row.stage() {
                        Some(stage) => format!(" {} · {stage} · full detail ", row.label()),
                        None => format!(" {} · full detail ", row.label()),
                    };
                    let detail = Paragraph::new(row.detail())
                        .block(Block::default().borders(Borders::ALL).title(title))
                        .wrap(Wrap { trim: false })
                        .scroll((detail_scroll, 0));
                    frame.render_widget(detail, detail_area);
                }

                let footer_area = areas[areas.len() - 1];
                let help = if completed && details_open {
                    "Completed · ↑↓/wheel scroll · Esc/Enter close detail · q closes viewer"
                } else if completed {
                    "Completed · Enter or click expands · q closes viewer · auto-closes if untouched"
                } else if details_open {
                    "↑↓/wheel scroll · Esc/Enter close · click another cell · Ctrl-C detach"
                } else {
                    "↑↓ select · Enter or click expands · Ctrl-C detaches; the job keeps running"
                };
                frame.render_widget(
                    Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
                    footer_area,
                );
            })
            .context("draw Factory live view")?;
        self.list_inner = list_inner;
        self.visible_offset = visible_offset;
        Ok(())
    }

    pub(crate) fn wait_for_action(
        &mut self,
        timeout: Duration,
        row_count: usize,
    ) -> Result<LiveAction> {
        if !event::poll(timeout).context("poll terminal input")? {
            return Ok(LiveAction::Continue);
        }
        loop {
            let action = match event::read().context("read terminal input")? {
                Event::Key(key) => self.handle_key(key, row_count),
                Event::Mouse(mouse) => self.handle_mouse(mouse, row_count),
                Event::Resize(_, _) => LiveAction::Continue,
                _ => LiveAction::Continue,
            };
            if matches!(action, LiveAction::Detach) || !event::poll(Duration::ZERO)? {
                return Ok(action);
            }
        }
    }

    pub(crate) fn inspect_completed(
        &mut self,
        job: &DurableJob,
        transcript: &Transcript,
        grace: Duration,
    ) -> Result<()> {
        self.draw(job, transcript, true)?;
        let initial_wait = if self.details_open || !self.follow_tail {
            Duration::from_secs(30)
        } else {
            grace
        };
        let mut deadline = Instant::now() + initial_wait;
        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            if !event::poll(timeout).context("poll completed-view input")? {
                return Ok(());
            }
            let terminal_event = event::read().context("read completed-view input")?;
            if completion_close_event(&terminal_event) {
                return Ok(());
            }
            if !completion_interaction(&terminal_event) {
                if matches!(terminal_event, Event::Resize(_, _)) {
                    self.draw(job, transcript, true)?;
                }
                continue;
            }
            deadline = Instant::now() + Duration::from_secs(30);
            match terminal_event {
                Event::Key(key) => {
                    self.handle_key(key, transcript.rows().len());
                }
                Event::Mouse(mouse) => {
                    self.handle_mouse(mouse, transcript.rows().len());
                }
                _ => unreachable!("filtered completion interaction"),
            }
            self.draw(job, transcript, true)?;
        }
    }

    pub(crate) fn restore(&mut self) -> Result<()> {
        let Some(mut terminal) = self.terminal.take() else {
            return Ok(());
        };
        let mut failure = disable_raw_mode()
            .context("disable terminal raw mode")
            .err();
        if let Err(error) = execute!(
            terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen,
            Show
        )
        .context("close Factory live view")
        {
            failure.get_or_insert(error);
        }
        if let Err(error) = terminal.show_cursor().context("restore terminal cursor") {
            failure.get_or_insert(error);
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn sync_selection(&mut self, row_count: usize) {
        if row_count == 0 {
            self.selected = None;
            self.details_open = false;
        } else if self.follow_tail || self.selected.is_none_or(|selected| selected >= row_count) {
            self.selected = Some(row_count - 1);
        }
    }

    fn handle_key(&mut self, key: KeyEvent, row_count: usize) -> LiveAction {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return LiveAction::Continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return LiveAction::Detach;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if self.details_open => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') if self.details_open => {
                self.detail_scroll = self.detail_scroll.saturating_add(1)
            }
            KeyCode::Home if self.details_open => self.detail_scroll = 0,
            KeyCode::PageUp if self.details_open => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10)
            }
            KeyCode::PageDown if self.details_open => {
                self.detail_scroll = self.detail_scroll.saturating_add(10)
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1, row_count),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1, row_count),
            KeyCode::Home => self.select(0, row_count),
            KeyCode::End => self.select(row_count.saturating_sub(1), row_count),
            KeyCode::Enter | KeyCode::Char(' ') if row_count > 0 => {
                self.details_open = !self.details_open;
                self.detail_scroll = 0;
                self.follow_tail = !self.details_open;
            }
            KeyCode::Esc => {
                self.details_open = false;
                self.detail_scroll = 0;
                self.follow_tail = true;
                self.sync_selection(row_count);
            }
            _ => {}
        }
        LiveAction::Continue
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, row_count: usize) -> LiveAction {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left)
                if self.list_inner.contains((mouse.column, mouse.row).into()) =>
            {
                let index =
                    self.visible_offset + usize::from(mouse.row.saturating_sub(self.list_inner.y));
                if index < row_count {
                    self.selected = Some(index);
                    self.follow_tail = false;
                    self.details_open = true;
                    self.detail_scroll = 0;
                }
            }
            MouseEventKind::ScrollUp if self.details_open => {
                self.detail_scroll = self.detail_scroll.saturating_sub(3)
            }
            MouseEventKind::ScrollDown if self.details_open => {
                self.detail_scroll = self.detail_scroll.saturating_add(3)
            }
            MouseEventKind::ScrollUp => self.move_selection(-3, row_count),
            MouseEventKind::ScrollDown => self.move_selection(3, row_count),
            _ => {}
        }
        LiveAction::Continue
    }

    fn move_selection(&mut self, delta: isize, row_count: usize) {
        if row_count == 0 {
            return;
        }
        let current = self.selected.unwrap_or(row_count - 1);
        let next = current
            .saturating_add_signed(delta)
            .min(row_count.saturating_sub(1));
        self.selected = Some(next);
        self.follow_tail = next == row_count - 1 && !self.details_open;
        self.detail_scroll = 0;
    }

    fn select(&mut self, index: usize, row_count: usize) {
        if row_count == 0 {
            return;
        }
        let index = index.min(row_count - 1);
        self.selected = Some(index);
        self.follow_tail = index == row_count - 1 && !self.details_open;
        self.detail_scroll = 0;
    }
}

impl Drop for LiveScreen {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn completion_close_event(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return false;
    }
    key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn completion_interaction(event: &Event) -> bool {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Enter
                    | KeyCode::Esc
                    | KeyCode::Char(' ' | 'j' | 'k')
            )
        }
        Event::Mouse(mouse) => matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
        ),
        _ => false,
    }
}

fn render_header(frame: &mut ratatui::Frame<'_>, area: Rect, job: &DurableJob) {
    let mut operations = job.operations.iter().collect::<Vec<_>>();
    operations.sort_by_key(|operation| operation.ordinal);
    let stage_line = operations
        .into_iter()
        .flat_map(|operation| {
            let label = operation
                .kind
                .strip_prefix("codex.")
                .unwrap_or(&operation.kind);
            let (symbol, color) = operation_marker(operation.state);
            [
                Span::styled(format!("{symbol} {label}"), Style::default().fg(color)),
                Span::raw("   "),
            ]
        })
        .collect::<Vec<_>>();
    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            format!(
                "Job {} · {}",
                job.job.job_id,
                job_state_label(job.job.state)
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(stage_line),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Software Factory "),
    );
    frame.render_widget(header, area);
}

fn tone_style(tone: EventTone) -> Style {
    let color = match tone {
        EventTone::Complete => Color::Green,
        EventTone::Error => Color::Red,
        EventTone::Muted => Color::DarkGray,
        EventTone::Running => Color::Cyan,
        EventTone::Warning => Color::Yellow,
    };
    Style::default().fg(color)
}

fn tone_symbol(tone: EventTone) -> &'static str {
    match tone {
        EventTone::Complete => "✓",
        EventTone::Error => "×",
        EventTone::Muted => "·",
        EventTone::Running => "›",
        EventTone::Warning => "!",
    }
}

fn operation_marker(state: OperationState) -> (&'static str, Color) {
    match state {
        OperationState::Ready => ("○", Color::DarkGray),
        OperationState::Running => ("●", Color::Cyan),
        OperationState::RetryWait => ("↻", Color::Yellow),
        OperationState::Succeeded => ("✓", Color::Green),
        OperationState::Failed => ("×", Color::Red),
        OperationState::Cancelled => ("-", Color::DarkGray),
    }
}

fn job_state_label(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Cancelling => "cancelling",
        JobState::Succeeded => "succeeded",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}
