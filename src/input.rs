//! Keyboard input handling: maps key events to state transitions.
//!
//! Pane focus is driven by number keys (1=Controls, 2=Catalog, 3=Query,
//! 4=Data) from the navigation panes. Text-entry contexts (the Controls field,
//! the in-place cell editor, and the query editor in insert mode) consume those
//! digits as input, so you leave them with `Esc` first.

use edtui::EditorMode;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::editor::{CellEditor, TextInput};
use crate::app::state::{App, ControlsField, Focus, Screen};
use crate::model::value::{TypeAffinity, Value};

/// Handle one key event, mutating the application state.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }

    // Ctrl+C always quits, from any screen or mode.
    if is_ctrl(&key, 'c') {
        app.quit();
        return;
    }

    match &app.screen {
        Screen::Picker { .. } => handle_picker(app, key),
        Screen::Connecting => handle_connecting(app, key),
        Screen::Fatal(_) => handle_fatal(app, key),
        Screen::Browser => handle_browser(app, key),
    }
}

fn handle_picker(app: &mut App, key: KeyEvent) {
    let Screen::Picker { selected } = &mut app.screen else {
        return;
    };
    let count = app.config.connections.len();
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Up | KeyCode::Char('k') => {
            *selected = selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') if *selected + 1 < count => {
            *selected += 1;
        }
        KeyCode::Enter => {
            let index = *selected;
            app.start_connection(index);
        }
        _ => {}
    }
}

fn handle_connecting(app: &mut App, key: KeyEvent) {
    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
        app.quit();
    }
}

fn handle_fatal(app: &mut App, key: KeyEvent) {
    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter) {
        app.should_quit = true;
    }
}

fn handle_browser(app: &mut App, key: KeyEvent) {
    // Ctrl+R runs the query pane from anywhere (intercepted before edtui).
    if is_ctrl(&key, 'r') {
        app.run_query_pane();
        return;
    }
    // Ctrl+S commits pending grid edits, except while editing a cell.
    if is_ctrl(&key, 's') && !matches!(app.browser.focus, Focus::CellEdit(_)) {
        app.commit_pending();
        return;
    }

    match &app.browser.focus {
        Focus::Controls { .. } => handle_controls(app, key),
        Focus::Catalog => handle_catalog(app, key),
        Focus::Query => handle_query(app, key),
        Focus::Data => handle_data(app, key),
        Focus::CellEdit(_) => handle_cell_edit(app, key),
        Focus::TableFinder(_) => handle_finder(app, key),
    }
}

fn handle_controls(app: &mut App, key: KeyEvent) {
    // Readline-style edits first.
    if is_ctrl(&key, 'w') {
        with_controls_input(app, TextInput::delete_word);
        return;
    }
    if is_ctrl(&key, 'u') {
        with_controls_input(app, TextInput::delete_to_start);
        return;
    }

    match key.code {
        KeyCode::Esc => app.focus_pane(4),
        KeyCode::Enter => {
            if let Some((field, text)) = current_controls(app) {
                app.apply_controls(field, text);
                app.focus_pane(4);
            }
        }
        KeyCode::Tab => {
            if let Some((field, text)) = current_controls(app) {
                app.apply_controls(field, text);
                let next = match field {
                    ControlsField::Filter => ControlsField::Order,
                    ControlsField::Order => ControlsField::Filter,
                };
                let seed = match next {
                    ControlsField::Filter => app.browser.filter_text.clone(),
                    ControlsField::Order => app.browser.order_text.clone(),
                };
                app.browser.focus = Focus::Controls {
                    field: next,
                    input: TextInput::with_text(&seed),
                };
            }
        }
        KeyCode::Backspace => with_controls_input(app, TextInput::backspace),
        KeyCode::Delete => with_controls_input(app, TextInput::delete),
        KeyCode::Left => with_controls_input(app, TextInput::left),
        KeyCode::Right => with_controls_input(app, TextInput::right),
        KeyCode::Home => with_controls_input(app, TextInput::home),
        KeyCode::End => with_controls_input(app, TextInput::end),
        KeyCode::Char(c) => with_controls_input(app, |input| input.insert(c)),
        _ => {}
    }
}

fn handle_catalog(app: &mut App, key: KeyEvent) {
    if let Some(pane) = pane_digit(&key) {
        app.focus_pane(pane);
        return;
    }
    match key.code {
        KeyCode::Char('q') => app.quit(),
        KeyCode::Down | KeyCode::Char('j') => {
            let count = app.browser.sidebar.names.len();
            let sel = &mut app.browser.sidebar.selected;
            if *sel + 1 < count {
                *sel += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.browser.sidebar.selected = app.browser.sidebar.selected.saturating_sub(1);
        }
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => app.open_sidebar_selection(),
        KeyCode::Char('/') => app.open_finder(),
        _ => {}
    }
}

fn handle_data(app: &mut App, key: KeyEvent) {
    if let Some(pane) = pane_digit(&key) {
        app.focus_pane(pane);
        return;
    }

    // Resolve a pending `g` (for `gg`) unless this key is another `g`.
    let is_g = matches!(key.code, KeyCode::Char('g'));
    if app.browser.awaiting_g && !is_g {
        app.browser.awaiting_g = false;
    }

    match key.code {
        KeyCode::Char('q') => app.quit(),
        KeyCode::Char('h') | KeyCode::Left => app.browser.grid.move_col(-1),
        KeyCode::Char('l') | KeyCode::Right => app.browser.grid.move_col(1),
        KeyCode::Char('j') | KeyCode::Down => app.browser.grid.move_row(1),
        KeyCode::Char('k') | KeyCode::Up => app.browser.grid.move_row(-1),
        KeyCode::Char('g') => {
            if app.browser.awaiting_g {
                app.browser.grid.goto_top();
                app.browser.awaiting_g = false;
            } else {
                app.browser.awaiting_g = true;
            }
        }
        KeyCode::Char('G') => app.browser.grid.goto_bottom(),
        KeyCode::Char('i') | KeyCode::Enter => begin_cell_edit(app),
        KeyCode::Char('u') => app.discard_pending(),
        KeyCode::Char('y') => app.yank_cell(),
        KeyCode::Char('Y') => app.yank_row(),
        KeyCode::Char('/') => app.open_finder(),
        _ => {}
    }
}

fn handle_query(app: &mut App, key: KeyEvent) {
    // In Normal mode the editor doesn't consume digits/Esc, so we use them for
    // pane switching and blurring. In every other mode all keys go to edtui
    // (Esc there returns to Normal).
    if app.browser.query.state.mode == EditorMode::Normal {
        if let Some(pane) = pane_digit(&key) {
            app.focus_pane(pane);
            return;
        }
        if key.code == KeyCode::Esc {
            app.focus_pane(4);
            return;
        }
    }
    let query = &mut app.browser.query;
    query.events.on_key_event(key, &mut query.state);
}

fn handle_cell_edit(app: &mut App, key: KeyEvent) {
    if is_ctrl(&key, 'w') {
        with_cell_input(app, TextInput::delete_word);
        return;
    }
    if is_ctrl(&key, 'u') {
        with_cell_input(app, TextInput::delete_to_start);
        return;
    }

    match key.code {
        // Esc and Enter both finalise the edit (per the spec, Esc saves/blurs).
        KeyCode::Esc | KeyCode::Enter => finish_cell_edit(app),
        KeyCode::Backspace => with_cell_input(app, TextInput::backspace),
        KeyCode::Delete => with_cell_input(app, TextInput::delete),
        KeyCode::Left => with_cell_input(app, TextInput::left),
        KeyCode::Right => with_cell_input(app, TextInput::right),
        KeyCode::Home => with_cell_input(app, TextInput::home),
        KeyCode::End => with_cell_input(app, TextInput::end),
        KeyCode::Char(c) => with_cell_input(app, |input| input.insert(c)),
        _ => {}
    }
}

fn handle_finder(app: &mut App, key: KeyEvent) {
    if is_ctrl(&key, 'w') {
        with_finder_query(app, TextInput::delete_word);
        return;
    }
    if is_ctrl(&key, 'u') {
        with_finder_query(app, TextInput::delete_to_start);
        return;
    }
    // fzf-style navigation.
    if is_ctrl(&key, 'n') {
        move_finder(app, 1);
        return;
    }
    if is_ctrl(&key, 'p') {
        move_finder(app, -1);
        return;
    }

    match key.code {
        KeyCode::Esc => app.finder_cancel(),
        KeyCode::Enter => app.finder_accept(),
        KeyCode::Down => move_finder(app, 1),
        KeyCode::Up => move_finder(app, -1),
        KeyCode::Backspace => with_finder_query(app, TextInput::backspace),
        KeyCode::Delete => with_finder_query(app, TextInput::delete),
        // Cursor motion does not change the query, so it must not re-rank.
        KeyCode::Left => with_finder_cursor(app, TextInput::left),
        KeyCode::Right => with_finder_cursor(app, TextInput::right),
        KeyCode::Home => with_finder_cursor(app, TextInput::home),
        KeyCode::End => with_finder_cursor(app, TextInput::end),
        KeyCode::Char(c) => with_finder_query(app, |input| input.insert(c)),
        _ => {}
    }
}

// --- helpers ---------------------------------------------------------------

fn is_ctrl(key: &KeyEvent, c: char) -> bool {
    key.code == KeyCode::Char(c) && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Map a bare `1`-`4` keypress to a pane number (ignoring Ctrl-modified digits).
fn pane_digit(key: &KeyEvent) -> Option<u8> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char(c @ '1'..='4') => Some(c as u8 - b'0'),
        _ => None,
    }
}

fn current_controls(app: &App) -> Option<(ControlsField, String)> {
    if let Focus::Controls { field, input } = &app.browser.focus {
        Some((*field, input.text()))
    } else {
        None
    }
}

fn with_controls_input(app: &mut App, op: impl FnOnce(&mut TextInput)) {
    if let Focus::Controls { input, .. } = &mut app.browser.focus {
        op(input);
    }
}

fn with_cell_input(app: &mut App, op: impl FnOnce(&mut TextInput)) {
    if let Focus::CellEdit(editor) = &mut app.browser.focus {
        op(&mut editor.input);
    }
}

/// Apply an edit to the finder query and re-rank the matches.
fn with_finder_query(app: &mut App, op: impl FnOnce(&mut TextInput)) {
    if let Focus::TableFinder(finder) = &mut app.browser.focus {
        op(&mut finder.input);
        finder.recompute();
    }
}

/// Move the finder query cursor without re-ranking (the query is unchanged).
fn with_finder_cursor(app: &mut App, op: impl FnOnce(&mut TextInput)) {
    if let Focus::TableFinder(finder) = &mut app.browser.focus {
        op(&mut finder.input);
    }
}

fn move_finder(app: &mut App, delta: isize) {
    if let Focus::TableFinder(finder) = &mut app.browser.focus {
        finder.move_selection(delta);
    }
}

fn begin_cell_edit(app: &mut App) {
    let grid = &app.browser.grid;
    if grid.read_only {
        app.error("Read-only (custom-query results and views can't be edited)");
        return;
    }
    if grid.row_count() == 0 || grid.col_count() == 0 {
        return;
    }
    let (row, col) = (grid.sel_row, grid.sel_col);
    let seed = match grid.display_value(row, col) {
        Some(value) if !value.is_null() => value.to_string(),
        _ => String::new(),
    };
    app.browser.focus = Focus::CellEdit(CellEditor::new(row, col, &seed));
}

fn finish_cell_edit(app: &mut App) {
    let Focus::CellEdit(editor) = &app.browser.focus else {
        return;
    };
    let (row, col) = (editor.row, editor.col);
    let text = editor.input.text();
    let affinity = app
        .browser
        .grid
        .columns
        .get(col)
        .map(|c| c.affinity)
        .unwrap_or(TypeAffinity::Unknown);
    let value = Value::parse(&text, affinity);
    app.browser.grid.record_edit(row, col, value);
    app.browser.focus = Focus::Data;
}
