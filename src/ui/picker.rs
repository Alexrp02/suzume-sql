//! Rendering for the connection picker overlay: a fuzzy-filterable list of the
//! configured connections, plus a footer that doubles as the confirmation
//! prompt for destructive actions.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::picker::{PickerPrompt, PickerState};
use crate::app::state::App;

use super::{centered_rect, input_spans};

pub fn render(frame: &mut Frame, app: &App, picker: &PickerState) {
    let area = centered_rect(60, 60, frame.area());
    frame.render_widget(Clear, area);

    let title = format!(
        " Connections — {}/{} ",
        picker.finder.match_count(),
        picker.finder.total_count()
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

    let [input_area, list_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner);

    let mut prompt = vec![Span::styled("› ", Style::default().fg(Color::Cyan))];
    prompt.extend(input_spans(&picker.finder.input));
    frame.render_widget(Paragraph::new(Line::from(prompt)), input_area);

    let items: Vec<ListItem> = picker
        .finder
        .matched_indices()
        .iter()
        .filter_map(|&i| app.config.connections.get(i))
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
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");

    let mut state = ListState::default();
    if picker.finder.match_count() > 0 {
        state.select(Some(picker.finder.selected_position()));
    }
    frame.render_stateful_widget(list, list_area, &mut state);

    frame.render_widget(Paragraph::new(footer(app, picker)), footer_area);
}

/// The picker's footer line: a pending confirmation, else a save error, else the
/// key hints.
fn footer(app: &App, picker: &PickerState) -> Line<'static> {
    match picker.prompt {
        Some(PickerPrompt::ConfirmDelete(index)) => {
            let name = app
                .config
                .connections
                .get(index)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            Line::from(Span::styled(
                format!("Delete `{name}`? Ctrl+X to confirm · Esc cancel"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ))
        }
        Some(PickerPrompt::ConfirmSwitch(_)) => Line::from(Span::styled(
            "Discard pending edits and switch? Enter to confirm · Esc cancel".to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        None if app.status.is_error => Line::from(Span::styled(
            app.status.message.clone(),
            Style::default().fg(Color::Red),
        )),
        None => Line::from(Span::styled(
            "Enter connect · Ctrl+A new · Ctrl+E edit · Ctrl+X delete · type to filter".to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    }
}
