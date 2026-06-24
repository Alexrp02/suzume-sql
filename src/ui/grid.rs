//! The virtualized data grid renderer.
//!
//! Rendering is stateless with respect to scrolling: the visible row window and
//! the first visible column are derived from the selection and the viewport
//! size every frame, so there is no scroll offset to keep in sync.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::editor::CellEditor;
use crate::app::state::{App, Focus};

const SEP: &str = " │ ";
const MIN_COL_WIDTH: usize = 3;
const MAX_COL_WIDTH: usize = 40;

const AMBER: Color = Color::Yellow;

pub fn render(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let grid = &app.browser.grid;
    let title = match &app.browser.loaded_table {
        Some(t) if grid.read_only => format!(" 4 {t} (read-only) "),
        Some(t) => format!(" 4 {t} "),
        // No table loaded: either a read-only custom-query result, or nothing yet.
        None if grid.col_count() > 0 => " 4 query result (read-only) ".to_string(),
        None => " 4 Data ".to_string(),
    };
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if grid.col_count() == 0 {
        frame.render_widget(Paragraph::new("No columns."), inner);
        return;
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The cell currently being edited, if any.
    let editing: Option<&CellEditor> = match &app.browser.focus {
        Focus::CellEdit(editor) => Some(editor),
        _ => None,
    };

    // Column widths from header + visible content.
    let widths = column_widths(app);

    // Choose the first visible column so the selected column stays on screen.
    let avail = inner.width as usize;
    let first_col = first_visible_column(&widths, grid.sel_col, avail);
    let visible_cols = visible_columns(&widths, first_col, avail);

    // Header.
    let mut lines: Vec<Line> = Vec::new();
    lines.push(header_line(app, &widths, first_col, visible_cols));

    // Vertical window.
    let body_height = inner.height.saturating_sub(1) as usize; // minus header
    if grid.row_count() == 0 {
        lines.push(Line::from(Span::styled(
            "(no rows)",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let row_offset = if grid.sel_row >= body_height {
        grid.sel_row - body_height + 1
    } else {
        0
    };
    let last_row = (row_offset + body_height).min(grid.row_count());

    for row in row_offset..last_row {
        lines.push(data_line(
            app,
            &widths,
            first_col,
            visible_cols,
            row,
            focused,
            editing,
        ));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn column_widths(app: &App) -> Vec<usize> {
    let grid = &app.browser.grid;
    grid.columns
        .iter()
        .enumerate()
        .map(|(col, meta)| {
            let mut width = meta.name.chars().count();
            for row in 0..grid.row_count() {
                if let Some(value) = grid.display_value(row, col) {
                    width = width.max(value.to_string().chars().count());
                }
            }
            width.clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
        })
        .collect()
}

fn first_visible_column(widths: &[usize], sel_col: usize, avail: usize) -> usize {
    let mut offset = 0;
    loop {
        let total: usize = (offset..=sel_col)
            .map(|i| widths.get(i).copied().unwrap_or(0) + SEP.len())
            .sum();
        if total <= avail || offset >= sel_col {
            break;
        }
        offset += 1;
    }
    offset
}

fn visible_columns(widths: &[usize], first_col: usize, avail: usize) -> usize {
    let mut used = 0usize;
    let mut count = 0usize;
    for (i, w) in widths.iter().enumerate().skip(first_col) {
        let add = if i == first_col { *w } else { *w + SEP.len() };
        if used + add > avail && count > 0 {
            break;
        }
        used += add;
        count += 1;
    }
    count.max(1)
}

fn header_line(app: &App, widths: &[usize], first_col: usize, visible_cols: usize) -> Line<'static> {
    let grid = &app.browser.grid;
    let mut spans: Vec<Span> = Vec::new();
    for (idx, col) in (first_col..first_col + visible_cols).enumerate() {
        if idx > 0 {
            spans.push(Span::raw(SEP));
        }
        let Some(meta) = grid.columns.get(col) else {
            continue;
        };
        let width = widths.get(col).copied().unwrap_or(MIN_COL_WIDTH);
        let mut style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        if meta.is_primary_key {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        spans.push(Span::styled(fit(&meta.name, width), style));
    }
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn data_line(
    app: &App,
    widths: &[usize],
    first_col: usize,
    visible_cols: usize,
    row: usize,
    focused: bool,
    editing: Option<&CellEditor>,
) -> Line<'static> {
    let grid = &app.browser.grid;
    let pending_delete = grid.is_pending_delete(row);
    let sep_style = if pending_delete {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    };
    let mut spans: Vec<Span> = Vec::new();

    for (idx, col) in (first_col..first_col + visible_cols).enumerate() {
        if idx > 0 {
            spans.push(Span::styled(SEP, sep_style));
        }
        let width = widths.get(col).copied().unwrap_or(MIN_COL_WIDTH);
        let is_selected = row == grid.sel_row && col == grid.sel_col;

        // In-place editing cell.
        if let Some(editor) = editing
            && editor.row == row
            && editor.col == col
        {
            spans.extend(edit_cell_spans(editor, width));
            continue;
        }

        let value = grid.display_value(row, col);
        let is_null = value.map(|v| v.is_null()).unwrap_or(true);
        let text = value.map(|v| v.to_string()).unwrap_or_default();
        let dirty = grid.is_dirty(row, col);

        let mut style = Style::default();
        if is_null {
            style = style.fg(Color::DarkGray).add_modifier(Modifier::ITALIC);
        }
        if dirty {
            style = style.fg(AMBER).add_modifier(Modifier::BOLD);
        }
        // A row marked for deletion overrides any per-cell colouring.
        if pending_delete {
            style = Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::CROSSED_OUT);
        }
        if is_selected && focused {
            style = style.add_modifier(Modifier::REVERSED);
        } else if is_selected {
            style = style.bg(Color::Rgb(40, 40, 40));
        }
        spans.push(Span::styled(fit(&text, width), style));
    }

    Line::from(spans)
}

/// Spans for the cell being edited, including a block cursor.
fn edit_cell_spans(editor: &CellEditor, width: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = editor.input.text().chars().collect();
    let cursor = editor.input.cursor();

    // Scroll the buffer so the cursor is always within the cell width.
    let start = if cursor >= width {
        cursor - width + 1
    } else {
        0
    };
    let end = (start + width).min(chars.len());

    let base = Style::default().bg(Color::Blue).fg(Color::White);
    let cursor_style = base.add_modifier(Modifier::REVERSED);

    let mut spans: Vec<Span> = Vec::new();
    let mut rendered = 0usize;
    for (i, ch) in chars[start..end].iter().enumerate() {
        let global = start + i;
        let style = if global == cursor { cursor_style } else { base };
        spans.push(Span::styled(ch.to_string(), style));
        rendered += 1;
    }
    // Cursor at end-of-buffer: draw a reversed space.
    if cursor >= chars.len() && rendered < width {
        spans.push(Span::styled(" ", cursor_style));
        rendered += 1;
    }
    // Pad the rest of the cell.
    if rendered < width {
        spans.push(Span::styled(" ".repeat(width - rendered), base));
    }
    spans
}

/// Fit a string into exactly `width` display columns, padding with spaces or
/// truncating with an ellipsis.
fn fit(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > width {
        if width <= 1 {
            return chars.iter().take(width).collect();
        }
        let mut out: String = chars[..width - 1].iter().collect();
        out.push('…');
        out
    } else {
        let mut out: String = chars.iter().collect();
        out.push_str(&" ".repeat(width - chars.len()));
        out
    }
}
