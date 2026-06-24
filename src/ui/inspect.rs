//! Rendering for the cell/row inspector overlay.
//!
//! Values are wrapped to the window width and scrolled vertically; the bottom
//! scroll bound is clamped here, where the wrapped line count is known.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::inspect::InspectTarget;
use crate::app::state::{App, Focus};
use crate::model::value::Value;

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = super::centered_rect(70, 70, frame.area());
    if area.width < 4 || area.height < 3 {
        return;
    }
    // Inner area inside the borders: drives both the wrap width and the viewport.
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    };

    let (title, lines) = {
        let Focus::Inspect(inspect) = &app.browser.focus else {
            return;
        };
        (title(&inspect.target), build_lines(&inspect.target, inner.width as usize))
    };

    let viewport = inner.height as usize;
    app.browser.inspect_viewport_rows = viewport;

    let max_scroll = u16::try_from(lines.len().saturating_sub(viewport)).unwrap_or(u16::MAX);
    let scroll = {
        let Focus::Inspect(inspect) = &mut app.browser.focus else {
            return;
        };
        inspect.scroll = inspect.scroll.min(max_scroll);
        inspect.scroll
    };

    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title)
        .title_bottom(" j/k scroll · Ctrl+D/U page · Esc close ");
    let paragraph = Paragraph::new(lines).block(block).scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn title(target: &InspectTarget) -> String {
    match target {
        InspectTarget::Cell { column, .. } => format!(" Inspect cell · {column} "),
        InspectTarget::Row { fields } => format!(" Inspect row · {} columns ", fields.len()),
    }
}

fn build_lines(target: &InspectTarget, width: usize) -> Vec<Line<'static>> {
    match target {
        InspectTarget::Cell { value, .. } => wrap_value(value, width.max(1))
            .into_iter()
            .map(Line::from)
            .collect(),
        InspectTarget::Row { fields } => {
            let mut lines = Vec::new();
            for (i, (name, value)) in fields.iter().enumerate() {
                if i > 0 {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled(
                    name.clone(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )));
                for span in wrap_value(value, width.saturating_sub(2).max(1)) {
                    lines.push(Line::from(vec![Span::raw("  "), span]));
                }
            }
            lines
        }
    }
}

/// Wrap a value's display text into styled spans, one per display line. `NULL`
/// renders dim and italic, matching the grid.
fn wrap_value(value: &Value, width: usize) -> Vec<Span<'static>> {
    let style = if value.is_null() {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default()
    };
    wrap_text(&value.to_string(), width)
        .into_iter()
        .map(|line| Span::styled(line, style))
        .collect()
}

/// Hard-wrap text to `width` characters, preserving existing newlines. Splits on
/// character boundaries (values are often single long tokens like JSON), so the
/// wrapped line count is exact for scroll clamping.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            let end = (start + width).min(chars.len());
            out.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_text_splits_on_width_and_keeps_newlines() {
        assert_eq!(wrap_text("abcdef", 3), vec!["abc", "def"]);
        assert_eq!(wrap_text("ab\ncd", 10), vec!["ab", "cd"]);
        // A trailing empty line (blank value row) is preserved as one line.
        assert_eq!(wrap_text("", 4), vec![""]);
    }

    #[test]
    fn cell_lines_count_matches_wrapped_height() {
        let value = Value::Text("0123456789".to_string());
        let target = InspectTarget::Cell {
            column: "data".to_string(),
            value,
        };
        // 10 chars wrapped at width 4 → 3 lines (4 + 4 + 2).
        assert_eq!(build_lines(&target, 4).len(), 3);
    }
}
