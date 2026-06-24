//! Rendering: the four-pane browser plus the picker/connecting/fatal screens.

mod grid;
mod inspect;

use edtui::{EditorMode, EditorTheme, EditorView};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::completion::{CandidateKind, Completion};
use crate::app::editor::TextInput;
use crate::app::finder::FinderState;
use crate::app::state::{App, ControlsField, Focus, Screen};
use crate::model::schema::RelationKind;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Height (incl. borders) of the query pane when unfocused vs focused.
const QUERY_HEIGHT_COMPACT: u16 = 3;
const QUERY_HEIGHT_FOCUSED: u16 = 9;

pub fn render(frame: &mut Frame, app: &mut App) {
    match &app.screen {
        Screen::Picker { selected } => {
            render_picker(frame, app, *selected);
        }
        Screen::Connecting => render_connecting(frame, app),
        Screen::Fatal(message) => {
            let message = message.clone();
            render_fatal(frame, &message);
        }
        Screen::Browser => render_browser(frame, app),
    }
}

fn render_browser(frame: &mut Frame, app: &mut App) {
    let [controls_area, main_area, status_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let [sidebar_area, right_area] =
        Layout::horizontal([Constraint::Length(28), Constraint::Min(0)]).areas(main_area);

    let query_focused = matches!(app.browser.focus, Focus::Query);
    let query_height = if query_focused {
        QUERY_HEIGHT_FOCUSED
    } else {
        QUERY_HEIGHT_COMPACT
    };
    let [query_area, data_area] =
        Layout::vertical([Constraint::Length(query_height), Constraint::Min(0)]).areas(right_area);

    // Record visible row counts for half-page scrolling: the grid loses 2 border
    // rows and 1 header row; the catalog list loses its 2 border rows.
    app.browser.grid_viewport_rows = (data_area.height as usize).saturating_sub(3);
    app.browser.catalog_viewport_rows = (sidebar_area.height as usize).saturating_sub(2);

    let active = app.browser.focus.pane();

    // Immutable renders first; the query editor needs `&mut`, so it goes last.
    render_controls(frame, controls_area, app, active == 1);
    render_sidebar(frame, sidebar_area, app, active == 2);
    grid::render(frame, data_area, app, active == 4);
    render_status(frame, status_area, app);
    render_query(frame, query_area, app, query_focused);

    // The completion popup floats just below whichever text field is focused.
    let completion_anchor = match active {
        1 => Some(controls_area),
        3 => Some(query_area),
        _ => None,
    };
    if let Some(completion) = &app.browser.completion
        && let Some(anchor) = completion_anchor
    {
        render_completion(frame, anchor, completion);
    }

    // The fuzzy finder overlays everything when active.
    if let Focus::TableFinder(finder) = &app.browser.focus {
        render_finder(frame, finder);
    }

    // The cell/row inspector overlays everything when active.
    if matches!(app.browser.focus, Focus::Inspect(_)) {
        inspect::render(frame, app);
    }
}

/// Render the autocompletion popup anchored under the query pane.
fn render_completion(frame: &mut Frame, query_area: Rect, completion: &Completion) {
    const MAX_ROWS: u16 = 8;
    let screen = frame.area();

    let visible = (completion.len() as u16).min(MAX_ROWS);
    let height = visible + 2; // borders
    let width = 40u16.min(query_area.width.max(10));

    // Anchor just below the query pane, indented to the editor text. Clamp into
    // the screen so a tall list near the bottom still renders.
    let x = (query_area.x + 2).min(screen.right().saturating_sub(width));
    let y_below = query_area.y + query_area.height;
    let y = y_below.min(screen.bottom().saturating_sub(height));

    let area = Rect {
        x,
        y,
        width,
        height,
    };
    if area.height < 3 || area.width < 4 {
        return;
    }

    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" complete ");

    let items: Vec<ListItem> = completion
        .items()
        .iter()
        .map(|item| {
            let (tag, tag_color) = match item.kind {
                CandidateKind::Table => ("table", Color::Green),
                CandidateKind::Column => ("col", Color::Yellow),
                CandidateKind::Keyword => ("kw", Color::Magenta),
            };
            ListItem::new(Line::from(vec![
                Span::raw(item.text.clone()),
                Span::raw(" "),
                Span::styled(format!("[{tag}]"), Style::default().fg(tag_color)),
            ]))
        })
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let mut state = ListState::default();
    if !completion.is_empty() {
        state.select(Some(completion.selected()));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn render_controls(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(" 1 Controls ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let table_name = app.browser.loaded_table.as_deref().unwrap_or("(none)");

    let (filter_input, order_input) = match &app.browser.focus {
        Focus::Controls {
            field: ControlsField::Filter,
            input,
        } => (Some(input), None),
        Focus::Controls {
            field: ControlsField::Order,
            input,
        } => (None, Some(input)),
        _ => (None, None),
    };

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(
        "Table: ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(table_name, Style::default().fg(Color::Green)));
    spans.push(Span::raw("   │   "));
    spans.extend(field_spans("Filter: ", &app.browser.filter_text, filter_input));
    spans.push(Span::raw("   │   "));
    spans.extend(field_spans("Order: ", &app.browser.order_text, order_input));

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

/// Render a labelled editable field. When `input` is `Some`, the field is
/// focused and shows its live buffer with a block cursor; otherwise it shows
/// the applied value (or a dim placeholder).
fn field_spans<'a>(label: &'a str, applied: &'a str, input: Option<&TextInput>) -> Vec<Span<'a>> {
    let mut spans = vec![Span::styled(
        label,
        Style::default().add_modifier(Modifier::BOLD),
    )];
    match input {
        Some(input) => {
            spans.push(Span::styled("[", Style::default().fg(Color::Cyan)));
            spans.extend(input_spans(input));
            spans.push(Span::styled("]", Style::default().fg(Color::Cyan)));
        }
        None => {
            if applied.is_empty() {
                spans.push(Span::styled("(any)", Style::default().fg(Color::DarkGray)));
            } else {
                spans.push(Span::raw(applied));
            }
        }
    }
    spans
}

fn input_spans(input: &TextInput) -> Vec<Span<'static>> {
    let chars: Vec<char> = input.text().chars().collect();
    let cursor = input.cursor();
    let normal = Style::default().fg(Color::White);
    let cursor_style = normal.add_modifier(Modifier::REVERSED);

    let mut spans: Vec<Span> = Vec::new();
    for (i, ch) in chars.iter().enumerate() {
        let style = if i == cursor { cursor_style } else { normal };
        spans.push(Span::styled(ch.to_string(), style));
    }
    if cursor >= chars.len() {
        spans.push(Span::styled(" ", cursor_style));
    }
    spans
}

fn render_sidebar(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(" 2 Catalog ");

    let items: Vec<ListItem> = app
        .browser
        .sidebar
        .names
        .iter()
        .map(|name| {
            let kind = app.catalog.find(name).map(|t| t.kind);
            let mut spans = vec![Span::raw(name.clone())];
            if matches!(kind, Some(RelationKind::View)) {
                spans.push(Span::styled(
                    " (view)",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");

    let mut state = ListState::default();
    if !app.browser.sidebar.names.is_empty() {
        state.select(Some(app.browser.sidebar.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_query(frame: &mut Frame, area: Rect, app: &mut App, focused: bool) {
    let mode_label = match app.browser.query.state.mode {
        EditorMode::Normal => "NORMAL",
        EditorMode::Insert => "INSERT",
        EditorMode::Visual => "VISUAL",
        EditorMode::Search => "SEARCH",
    };
    let title = format!(" 3 Query · {mode_label} · Ctrl+Space complete · Ctrl+R run ");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // edtui draws its own mode status line; we surface the mode in the title
    // instead so the compact (unfocused) pane keeps its single content line.
    let theme = EditorTheme::default()
        .hide_status_line()
        .base(Style::default());
    let view = EditorView::new(&mut app.browser.query.state).theme(theme);
    frame.render_widget(view, inner);
}

fn render_finder(frame: &mut Frame, finder: &FinderState) {
    let area = centered_rect(60, 60, frame.area());
    frame.render_widget(Clear, area);

    let title = format!(
        " Find table — {}/{} (Enter open · Esc cancel) ",
        finder.match_count(),
        finder.total_count()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let [input_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    let mut prompt = vec![Span::styled("› ", Style::default().fg(Color::Cyan))];
    prompt.extend(input_spans(&finder.input));
    frame.render_widget(Paragraph::new(Line::from(prompt)), input_area);

    let items: Vec<ListItem> = finder
        .matched_names()
        .into_iter()
        .map(|name| ListItem::new(name.to_string()))
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");

    let mut state = ListState::default();
    if finder.match_count() > 0 {
        state.select(Some(finder.selected_position()));
    }
    frame.render_stateful_widget(list, list_area, &mut state);
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();

    if let Some(op) = app.pending {
        let frame_char = SPINNER[app.spinner_frame % SPINNER.len()];
        spans.push(Span::styled(
            format!("{frame_char} {} ", op.label()),
            Style::default().fg(Color::Cyan),
        ));
    }

    let message_style = if app.status.is_error {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    spans.push(Span::styled(app.status.message.clone(), message_style));

    let pending = app.browser.grid.pending_count();
    if pending > 0 {
        spans.push(Span::styled(
            format!("  • {pending} pending [Ctrl+S commit / u discard]"),
            Style::default().fg(Color::Yellow),
        ));
    }

    spans.push(Span::styled(
        "   [1-4 panes · / find · i/I inspect · e edit · y/Y yank · Ctrl+R run · R refresh · q quit]",
        Style::default().fg(Color::DarkGray),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_picker(frame: &mut Frame, app: &App, selected: usize) {
    let area = centered_rect(60, 60, frame.area());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Select a connection ");

    let items: Vec<ListItem> = app
        .config
        .connections
        .iter()
        .map(|c| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    c.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  [{}] ", c.connection.engine_label()),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    c.connection.target().to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_connecting(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 20, frame.area());
    let frame_char = SPINNER[app.spinner_frame % SPINNER.len()];
    let text = format!("{frame_char}  {}", app.status.message);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
}

fn render_fatal(frame: &mut Frame, message: &str) {
    let area = centered_rect(70, 40, frame.area());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" Error ");
    let lines = vec![
        Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(Color::Red),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Press q to quit.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: true })
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
}

/// A rect centred within `area`, sized as a percentage of it.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, vertical, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);
    let [_, center, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(vertical);
    center
}
