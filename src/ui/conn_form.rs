//! Rendering for the connection create/edit form: a modal, vim-style overlay
//! with a name field, an engine selector, a target field, the connection-test
//! status line, and a mode-aware footer hint.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::conn_form::{ConnectionDraft, FormFocus, TestStatus};
use crate::app::editor::TextInput;

use super::{centered_rect, input_spans};

pub fn render(frame: &mut Frame, draft: &ConnectionDraft) {
    let area = centered_rect(60, 50, frame.area());
    frame.render_widget(Clear, area);

    let heading = if draft.editing.is_some() {
        " Edit connection "
    } else {
        " New connection "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(heading);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let [
        name_area,
        engine_area,
        value_area,
        _gap,
        test_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let name_editing = matches!(draft.focus, FormFocus::Name { editing: true });
    let value_editing = matches!(draft.focus, FormFocus::Value { editing: true });
    frame.render_widget(
        Paragraph::new(field_line(
            "Name",
            &draft.name,
            matches!(draft.focus, FormFocus::Name { .. }),
            name_editing,
        )),
        name_area,
    );
    frame.render_widget(
        Paragraph::new(engine_line(draft, draft.focus == FormFocus::Engine)),
        engine_area,
    );
    frame.render_widget(
        Paragraph::new(field_line(
            draft.engine.value_label(),
            &draft.value,
            matches!(draft.focus, FormFocus::Value { .. }),
            value_editing,
        )),
        value_area,
    );
    frame.render_widget(
        Paragraph::new(test_line(&draft.test)).wrap(Wrap { trim: true }),
        test_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint(draft.focus.is_editing()),
            Style::default().fg(Color::DarkGray),
        ))),
        footer_area,
    );
}

/// The mode-aware footer hint for the connection form.
fn hint(editing: bool) -> &'static str {
    if editing {
        "INSERT · Esc done · Enter next field · Ctrl+S save"
    } else {
        "NORMAL · j/k move · ←/→ engine · Enter edit · Ctrl+T test · Ctrl+S save · Esc cancel"
    }
}

fn label_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

/// A labelled field for the connection form. While editing (insert mode) it
/// shows the live buffer with a block cursor; when focused in normal mode the
/// value is highlighted; otherwise it shows the value (or a dim placeholder).
fn field_line(label: &str, input: &TextInput, focused: bool, editing: bool) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:>8}: "), label_style(focused))];
    if editing {
        spans.push(Span::styled("[", Style::default().fg(Color::Cyan)));
        spans.extend(input_spans(input));
        spans.push(Span::styled("]", Style::default().fg(Color::Cyan)));
        return Line::from(spans);
    }
    let text = input.text();
    let (body, empty) = if text.is_empty() {
        ("(empty)".to_string(), true)
    } else {
        (text, false)
    };
    let style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if empty {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    spans.push(Span::styled(body, style));
    Line::from(spans)
}

fn engine_line(draft: &ConnectionDraft, focused: bool) -> Line<'static> {
    let value_style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    Line::from(vec![
        Span::styled(format!("{:>8}: ", "Engine"), label_style(focused)),
        Span::styled(format!("‹ {} ›", draft.engine.label()), value_style),
    ])
}

fn test_line(status: &TestStatus) -> Line<'static> {
    match status {
        TestStatus::Idle => Line::from(""),
        TestStatus::Testing(_) => Line::from(Span::styled(
            "  Testing connection…".to_string(),
            Style::default().fg(Color::Cyan),
        )),
        TestStatus::Ok => Line::from(Span::styled(
            "  ✓ Connection succeeded".to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        TestStatus::Failed(msg) => Line::from(Span::styled(
            format!("  ✗ {msg}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
    }
}
