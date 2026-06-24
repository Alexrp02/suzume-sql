//! Application state and the top-level state machine.

use std::collections::HashMap;

use edtui::{EditorEventHandler, EditorState, Lines};

use crate::app::editor::{CellEditor, TextInput};
use crate::app::finder::FinderState;
use crate::config::Config;
use crate::db::query::SelectQuery;
use crate::model::delta::{KeyPart, CellDelta, RowKey, RowMutation};
use crate::model::schema::{Catalog, ColumnMeta};
use crate::model::value::Value;
use crate::worker::{WorkerHandle, WorkerRequest, WorkerResponse};

/// Default row cap for browse queries.
pub const ROW_LIMIT: u32 = 100;

/// The operation currently in flight on the worker, for the spinner/status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOp {
    Schema,
    Select,
    Commit,
}

impl PendingOp {
    pub fn label(&self) -> &'static str {
        match self {
            PendingOp::Schema => "Loading schema",
            PendingOp::Select => "Running query",
            PendingOp::Commit => "Committing",
        }
    }
}

/// A one-line status message.
#[derive(Debug, Clone, Default)]
pub struct StatusLine {
    pub message: String,
    pub is_error: bool,
}

/// Top-level screen. Each variant is a distinct, mutually exclusive state.
pub enum Screen {
    /// Choosing among multiple configured connections.
    Picker { selected: usize },
    /// Worker spawned; awaiting connection and first schema harvest.
    Connecting,
    /// The main three-pane browser.
    Browser,
    /// Unrecoverable error; the message is shown until the user quits.
    Fatal(String),
}

/// Which field of the Controls pane is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlsField {
    Filter,
    Order,
}

/// Which pane owns keyboard focus, and the transient editing state it carries.
///
/// Panes map to number keys: 1 = Controls, 2 = Catalog, 3 = Query, 4 = Data.
/// Editing buffers that are inherently transient (the Controls field input and
/// the in-place cell editor) live inside the focus variants, so e.g. "editing a
/// cell" cannot exist without a `CellEditor`. The query editor persists in
/// [`BrowserState`] because its pane is always shown, focused or not.
pub enum Focus {
    /// Pane 1: editing the filter or order field.
    Controls { field: ControlsField, input: TextInput },
    /// Pane 2: the catalog/table list.
    Catalog,
    /// Pane 3: the vim-style SQL query editor.
    Query,
    /// Pane 4: navigating the data grid.
    Data,
    /// Editing a single grid cell (a sub-state of the data grid).
    CellEdit(CellEditor),
    /// The fuzzy table finder overlay (summoned from Catalog/Data).
    TableFinder(FinderState),
}

impl Focus {
    /// The pane number this focus belongs to (for highlighting).
    pub fn pane(&self) -> u8 {
        match self {
            Focus::Controls { .. } => 1,
            // The finder overlays the catalog.
            Focus::Catalog | Focus::TableFinder(_) => 2,
            Focus::Query => 3,
            Focus::Data | Focus::CellEdit(_) => 4,
        }
    }
}

/// The catalog sidebar.
#[derive(Debug, Default)]
pub struct SidebarState {
    pub names: Vec<String>,
    pub selected: usize,
}

/// The virtualized data grid: original rows plus an overlay of pending edits.
pub struct GridView {
    pub columns: Vec<ColumnMeta>,
    /// Original values as returned by the last query.
    pub rows: Vec<Vec<Value>>,
    /// Pending edits keyed by `(row, col)`; the source of truth for what is
    /// displayed (amber) and what will be committed.
    pub overlay: HashMap<(usize, usize), Value>,
    pub sel_row: usize,
    pub sel_col: usize,
    /// Views and the no-table state are read-only: editing is disabled.
    pub read_only: bool,
}

impl GridView {
    pub fn empty() -> GridView {
        GridView {
            columns: Vec::new(),
            rows: Vec::new(),
            overlay: HashMap::new(),
            sel_row: 0,
            sel_col: 0,
            read_only: true,
        }
    }

    pub fn new(columns: Vec<ColumnMeta>, read_only: bool) -> GridView {
        GridView {
            columns,
            rows: Vec::new(),
            overlay: HashMap::new(),
            sel_row: 0,
            sel_col: 0,
            read_only,
        }
    }

    pub fn set_rows(&mut self, rows: Vec<Vec<Value>>) {
        self.rows = rows;
        self.overlay.clear();
        self.clamp_selection();
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn col_count(&self) -> usize {
        self.columns.len()
    }

    /// The value to display at `(row, col)`: the pending edit if any, else the
    /// original.
    pub fn display_value(&self, row: usize, col: usize) -> Option<&Value> {
        self.overlay
            .get(&(row, col))
            .or_else(|| self.rows.get(row).and_then(|r| r.get(col)))
    }

    pub fn is_dirty(&self, row: usize, col: usize) -> bool {
        self.overlay.contains_key(&(row, col))
    }

    pub fn pending_count(&self) -> usize {
        self.overlay.len()
    }

    pub fn has_pending(&self) -> bool {
        !self.overlay.is_empty()
    }

    /// Record an edit at `(row, col)`. Re-setting a cell to its original value
    /// clears the pending edit instead of recording a no-op.
    pub fn record_edit(&mut self, row: usize, col: usize, new_value: Value) {
        let original = self.rows.get(row).and_then(|r| r.get(col));
        match original {
            Some(orig) if *orig == new_value => {
                self.overlay.remove(&(row, col));
            }
            _ => {
                self.overlay.insert((row, col), new_value);
            }
        }
    }

    fn clamp_selection(&mut self) {
        if self.sel_row >= self.row_count() {
            self.sel_row = self.row_count().saturating_sub(1);
        }
        if self.sel_col >= self.col_count() {
            self.sel_col = self.col_count().saturating_sub(1);
        }
    }

    pub fn move_row(&mut self, delta: isize) {
        if self.row_count() == 0 {
            return;
        }
        let last = self.row_count() - 1;
        self.sel_row = clamp_add(self.sel_row, delta, last);
    }

    pub fn move_col(&mut self, delta: isize) {
        if self.col_count() == 0 {
            return;
        }
        let last = self.col_count() - 1;
        self.sel_col = clamp_add(self.sel_col, delta, last);
    }

    pub fn goto_top(&mut self) {
        self.sel_row = 0;
    }

    pub fn goto_bottom(&mut self) {
        self.sel_row = self.row_count().saturating_sub(1);
    }

    /// Compile the overlay into one [`RowMutation`] per edited row.
    pub fn build_mutations(&self, table: &str) -> Vec<RowMutation> {
        // Group overlay entries by row.
        let mut by_row: HashMap<usize, Vec<usize>> = HashMap::new();
        for (row, col) in self.overlay.keys() {
            by_row.entry(*row).or_default().push(*col);
        }

        let mut mutations = Vec::with_capacity(by_row.len());
        for (row, cols) in by_row {
            let Some(row_values) = self.rows.get(row) else {
                continue;
            };
            let key = self.row_key(row_values);
            let mut changes = Vec::with_capacity(cols.len());
            for col in cols {
                let (Some(column), Some(new_value), Some(original)) = (
                    self.columns.get(col),
                    self.overlay.get(&(row, col)),
                    row_values.get(col),
                ) else {
                    continue;
                };
                changes.push(CellDelta {
                    column: column.name.clone(),
                    original: original.clone(),
                    new: new_value.clone(),
                });
            }
            if !changes.is_empty() {
                mutations.push(RowMutation {
                    table: table.to_string(),
                    key,
                    changes,
                });
            }
        }
        mutations
    }

    /// Build the row-identifying key from the original row values: primary key
    /// when available, otherwise a full-row equality match.
    fn row_key(&self, row_values: &[Value]) -> RowKey {
        let pk_indices: Vec<usize> = self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_primary_key)
            .map(|(i, _)| i)
            .collect();

        if pk_indices.is_empty() {
            let parts = self
                .columns
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    row_values.get(i).map(|v| KeyPart {
                        column: c.name.clone(),
                        value: v.clone(),
                    })
                })
                .collect();
            RowKey::FullRow(parts)
        } else {
            let parts = pk_indices
                .into_iter()
                .filter_map(|i| {
                    let column = self.columns.get(i)?;
                    let value = row_values.get(i)?;
                    Some(KeyPart {
                        column: column.name.clone(),
                        value: value.clone(),
                    })
                })
                .collect();
            RowKey::PrimaryKey(parts)
        }
    }
}

fn clamp_add(current: usize, delta: isize, max: usize) -> usize {
    let next = current as isize + delta;
    if next < 0 {
        0
    } else if next as usize > max {
        max
    } else {
        next as usize
    }
}

/// The persistent vim-style SQL query editor (pane 3).
///
/// Both the buffer/cursor state and the key-sequence handler persist across
/// frames so multi-key motions (e.g. `dd`, counts) work, and the editor keeps
/// its content even while another pane is focused.
pub struct QueryPane {
    pub state: EditorState,
    pub events: EditorEventHandler,
}

impl QueryPane {
    fn new() -> QueryPane {
        QueryPane {
            state: EditorState::new(Lines::from("")),
            events: EditorEventHandler::default(),
        }
    }

    /// The current SQL text in the buffer.
    pub fn sql(&self) -> String {
        self.state.lines.to_string()
    }
}

/// The main browser pane state.
pub struct BrowserState {
    pub focus: Focus,
    pub sidebar: SidebarState,
    /// The applied filter (`WHERE` fragment).
    pub filter_text: String,
    /// The applied order (`ORDER BY` fragment).
    pub order_text: String,
    pub query: QueryPane,
    pub grid: GridView,
    /// The table currently loaded into the grid. `None` while showing the
    /// read-only result of a raw custom query.
    pub loaded_table: Option<String>,
    /// True after a single `g` in grid navigation, awaiting the second `g`.
    pub awaiting_g: bool,
}

impl BrowserState {
    fn new() -> BrowserState {
        BrowserState {
            focus: Focus::Catalog,
            sidebar: SidebarState::default(),
            filter_text: String::new(),
            order_text: String::new(),
            query: QueryPane::new(),
            grid: GridView::empty(),
            loaded_table: None,
            awaiting_g: false,
        }
    }
}

/// The whole application.
pub struct App {
    pub config: Config,
    pub screen: Screen,
    pub worker: Option<WorkerHandle>,
    pub connection_name: String,
    pub catalog: Catalog,
    pub browser: BrowserState,
    pub status: StatusLine,
    pub pending: Option<PendingOp>,
    pub spinner_frame: usize,
    pub should_quit: bool,
    select_seq: u64,
    latest_select_id: u64,
}

impl App {
    /// Build the app for `config`, honouring an optional pre-selected
    /// connection name (e.g. from the command line).
    pub fn new(config: Config, preselect: Option<String>) -> App {
        let mut app = App {
            config,
            screen: Screen::Picker { selected: 0 },
            worker: None,
            connection_name: String::new(),
            catalog: Catalog::default(),
            browser: BrowserState::new(),
            status: StatusLine::default(),
            pending: None,
            spinner_frame: 0,
            should_quit: false,
            select_seq: 0,
            latest_select_id: 0,
        };

        match preselect {
            Some(name) => match app.config.connection(&name) {
                Ok(_) => {
                    let index = app
                        .config
                        .connections
                        .iter()
                        .position(|c| c.name == name)
                        .unwrap_or(0);
                    app.start_connection(index);
                }
                Err(e) => app.screen = Screen::Fatal(e.to_string()),
            },
            None => {
                if app.config.connections.len() == 1 {
                    app.start_connection(0);
                }
            }
        }
        app
    }

    /// Spawn the worker for the connection at `index` and enter `Connecting`.
    pub fn start_connection(&mut self, index: usize) {
        let Some(named) = self.config.connections.get(index) else {
            self.screen = Screen::Fatal("invalid connection index".to_string());
            return;
        };
        self.connection_name = named.name.clone();
        self.worker = Some(WorkerHandle::spawn(named.connection.clone()));
        self.screen = Screen::Connecting;
        self.status = StatusLine {
            message: format!("Connecting to `{}`...", named.name),
            is_error: false,
        };
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.status = StatusLine {
            message: message.into(),
            is_error: false,
        };
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.status = StatusLine {
            message: message.into(),
            is_error: true,
        };
    }

    /// Drain and apply all responses currently queued from the worker.
    pub fn drain_worker(&mut self) {
        let responses = match &self.worker {
            Some(worker) => worker.try_recv(),
            None => Vec::new(),
        };
        self.apply_responses(responses);
    }

    /// Apply a batch of worker responses.
    pub fn apply_responses(&mut self, responses: Vec<WorkerResponse>) {
        for response in responses {
            self.apply_response(response);
        }
    }

    fn apply_response(&mut self, response: WorkerResponse) {
        match response {
            WorkerResponse::Connected => {
                self.send(WorkerRequest::HarvestSchema);
                self.pending = Some(PendingOp::Schema);
                self.info("Connected. Loading schema...");
            }
            WorkerResponse::Schema(catalog) => {
                self.on_schema(catalog);
            }
            WorkerResponse::Rows { id, rows } => {
                if id == self.latest_select_id {
                    self.pending = None;
                    let count = rows.len();
                    self.browser.grid.set_rows(rows);
                    self.info(format!("{count} row(s)"));
                }
            }
            WorkerResponse::RawRows {
                id,
                columns,
                rows,
                truncated,
            } => {
                if id == self.latest_select_id {
                    self.on_raw_rows(columns, rows, truncated);
                }
            }
            WorkerResponse::Committed => {
                self.pending = None;
                self.info("Changes committed");
                // Refresh to show the persisted state.
                self.run_current_select();
            }
            WorkerResponse::Failed(message) => {
                let was_connecting = matches!(self.screen, Screen::Connecting);
                self.pending = None;
                if was_connecting {
                    self.screen = Screen::Fatal(message);
                } else {
                    self.error(message);
                }
            }
        }
    }

    fn on_schema(&mut self, catalog: Catalog) {
        let names: Vec<String> = catalog.tables.iter().map(|t| t.name.clone()).collect();
        self.catalog = catalog;
        self.browser = BrowserState::new();
        self.browser.sidebar.names = names;
        self.screen = Screen::Browser;
        self.pending = None;
        if self.browser.sidebar.names.is_empty() {
            self.info("No tables or views found");
        } else {
            // Auto-load the first relation and focus the grid.
            self.open_sidebar_selection();
        }
    }

    /// Load the relation currently highlighted in the sidebar into the grid.
    pub fn open_sidebar_selection(&mut self) {
        let index = self.browser.sidebar.selected;
        let Some(name) = self.browser.sidebar.names.get(index).cloned() else {
            return;
        };
        let meta = self.catalog.find(&name);
        let columns = meta.map(|m| m.columns.clone()).unwrap_or_default();
        let read_only = meta.map(|m| !m.is_editable()).unwrap_or(true);
        self.browser.loaded_table = Some(name.clone());
        self.browser.grid = GridView::new(columns, read_only);
        self.browser.focus = Focus::Data;
        if read_only {
            self.info(format!("`{name}` is read-only"));
        }
        self.run_current_select();
    }

    /// Display the result of a raw custom query: always read-only, with columns
    /// discovered from the result set (no table/PK mapping).
    fn on_raw_rows(&mut self, columns: Vec<String>, rows: Vec<Vec<Value>>, truncated: bool) {
        self.pending = None;
        let synthetic_columns: Vec<ColumnMeta> = columns
            .into_iter()
            .map(|name| ColumnMeta {
                name,
                declared_type: String::new(),
                affinity: crate::model::value::TypeAffinity::Unknown,
                is_primary_key: false,
            })
            .collect();
        let count = rows.len();
        // A raw result is not tied to a table, so it cannot be committed.
        self.browser.loaded_table = None;
        self.browser.grid = GridView::new(synthetic_columns, true);
        self.browser.grid.set_rows(rows);
        // Move focus to the results so they can be inspected immediately.
        self.browser.focus = Focus::Data;
        if truncated {
            self.info(format!("{count} rows (capped at {})", crate::db::RAW_ROW_CAP));
        } else {
            self.info(format!("{count} row(s) from query"));
        }
    }

    /// Run the SQL currently in the query pane (read-only results).
    pub fn run_query_pane(&mut self) {
        let sql = self.browser.query.sql();
        if sql.trim().is_empty() {
            self.info("Query is empty");
            return;
        }
        self.select_seq += 1;
        self.latest_select_id = self.select_seq;
        self.send(WorkerRequest::RunRawQuery {
            id: self.latest_select_id,
            sql,
        });
        self.pending = Some(PendingOp::Select);
        self.info("Running query...");
    }

    /// Open the fuzzy table finder over the current catalog.
    pub fn open_finder(&mut self) {
        if self.browser.sidebar.names.is_empty() {
            self.info("No tables to search");
            return;
        }
        let names = self.browser.sidebar.names.clone();
        self.browser.focus = Focus::TableFinder(FinderState::new(names));
    }

    /// Accept the finder's selection: load that table and close the finder.
    pub fn finder_accept(&mut self) {
        let index = match &self.browser.focus {
            Focus::TableFinder(finder) => finder.selected_index(),
            _ => return,
        };
        match index {
            // `names` in the finder mirrors the sidebar ordering, so the index
            // is directly usable as the sidebar selection.
            Some(index) => {
                self.browser.sidebar.selected = index;
                self.open_sidebar_selection();
            }
            None => self.browser.focus = Focus::Catalog,
        }
    }

    /// Close the finder without changing the selection.
    pub fn finder_cancel(&mut self) {
        self.browser.focus = Focus::Catalog;
    }

    /// Focus a pane by its number (1=Controls, 2=Catalog, 3=Query, 4=Data).
    pub fn focus_pane(&mut self, pane: u8) {
        self.browser.awaiting_g = false;
        self.browser.focus = match pane {
            1 => Focus::Controls {
                field: ControlsField::Filter,
                input: TextInput::with_text(&self.browser.filter_text),
            },
            2 => Focus::Catalog,
            3 => Focus::Query,
            _ => Focus::Data,
        };
    }

    /// Re-run the browse query for the loaded table with current filter/order.
    pub fn run_current_select(&mut self) {
        let Some(table) = self.browser.loaded_table.clone() else {
            return;
        };
        let columns = self
            .catalog
            .find(&table)
            .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default();

        self.select_seq += 1;
        self.latest_select_id = self.select_seq;

        let query = SelectQuery {
            table,
            columns,
            filter: non_empty(&self.browser.filter_text),
            order_by: non_empty(&self.browser.order_text),
            limit: ROW_LIMIT,
        };
        self.send(WorkerRequest::RunSelect {
            id: self.latest_select_id,
            query,
        });
        self.pending = Some(PendingOp::Select);
    }

    /// Apply an edited Controls field and re-run the browse query.
    pub fn apply_controls(&mut self, field: ControlsField, text: String) {
        match field {
            ControlsField::Filter => self.browser.filter_text = text,
            ControlsField::Order => self.browser.order_text = text,
        }
        if self.browser.loaded_table.is_some() {
            self.run_current_select();
        } else {
            self.info("Select a table to apply a filter");
        }
    }

    /// Build and dispatch a commit for all pending edits.
    pub fn commit_pending(&mut self) {
        if self.browser.grid.read_only {
            self.error("This relation is read-only");
            return;
        }
        let Some(table) = self.browser.loaded_table.clone() else {
            return;
        };
        let mutations = self.browser.grid.build_mutations(&table);
        if mutations.is_empty() {
            self.info("Nothing to commit");
            return;
        }
        let count = mutations.len();
        self.send(WorkerRequest::Commit(mutations));
        self.pending = Some(PendingOp::Commit);
        self.info(format!("Committing {count} row(s)..."));
    }

    /// Discard all pending edits.
    pub fn discard_pending(&mut self) {
        if self.browser.grid.has_pending() {
            self.browser.grid.overlay.clear();
            self.info("Pending edits discarded");
        }
    }

    fn send(&self, request: WorkerRequest) {
        if let Some(worker) = &self.worker {
            worker.send(request);
        }
    }

    pub fn quit(&mut self) {
        self.send(WorkerRequest::Shutdown);
        self.should_quit = true;
    }
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::value::TypeAffinity;

    fn col(name: &str, pk: bool) -> ColumnMeta {
        ColumnMeta {
            name: name.to_string(),
            declared_type: "text".to_string(),
            affinity: TypeAffinity::Text,
            is_primary_key: pk,
        }
    }

    fn grid_with_pk() -> GridView {
        let mut grid = GridView::new(vec![col("id", true), col("email", false)], false);
        grid.set_rows(vec![
            vec![Value::Integer(1), Value::Text("a@x".to_string())],
            vec![Value::Integer(2), Value::Text("b@x".to_string())],
        ]);
        grid
    }

    #[test]
    fn edit_then_build_mutation_uses_primary_key() {
        let mut grid = grid_with_pk();
        grid.record_edit(0, 1, Value::Text("new@x".to_string()));
        assert!(grid.is_dirty(0, 1));

        let mutations = grid.build_mutations("users");
        assert_eq!(mutations.len(), 1);
        let m = &mutations[0];
        assert_eq!(m.table, "users");
        assert_eq!(
            m.key,
            RowKey::PrimaryKey(vec![KeyPart {
                column: "id".to_string(),
                value: Value::Integer(1),
            }])
        );
        assert_eq!(m.changes.len(), 1);
        assert_eq!(m.changes[0].original, Value::Text("a@x".to_string()));
        assert_eq!(m.changes[0].new, Value::Text("new@x".to_string()));
    }

    #[test]
    fn editing_back_to_original_clears_the_pending_edit() {
        let mut grid = grid_with_pk();
        grid.record_edit(0, 1, Value::Text("new@x".to_string()));
        grid.record_edit(0, 1, Value::Text("a@x".to_string()));
        assert!(!grid.is_dirty(0, 1));
        assert!(grid.build_mutations("users").is_empty());
    }

    #[test]
    fn renders_browser_on_a_tiny_viewport_without_panicking() {
        use crate::app::editor::CellEditor;
        use crate::config::Config;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut grid = GridView::new(vec![col("id", true), col("email", false)], false);
        grid.set_rows(vec![
            vec![Value::Integer(1), Value::Text("alex@dev.com".to_string())],
            vec![Value::Integer(2), Value::Null],
        ]);
        grid.record_edit(0, 1, Value::Text("new@x".to_string()));

        let browser = BrowserState {
            // Actively editing a cell exercises the edit-cursor render path.
            focus: Focus::CellEdit(CellEditor::new(0, 1, "new@x")),
            sidebar: SidebarState {
                names: vec!["users".to_string()],
                selected: 0,
            },
            filter_text: "age > 30".to_string(),
            order_text: String::new(),
            query: QueryPane::new(),
            grid,
            loaded_table: Some("users".to_string()),
            awaiting_g: false,
        };

        let mut app = App {
            config: Config {
                connections: Vec::new(),
            },
            screen: Screen::Browser,
            worker: None,
            connection_name: "local".to_string(),
            catalog: Catalog::default(),
            browser,
            status: StatusLine::default(),
            pending: Some(PendingOp::Select),
            spinner_frame: 3,
            should_quit: false,
            select_seq: 0,
            latest_select_id: 0,
        };

        // A deliberately cramped terminal stresses the column/scroll arithmetic.
        let mut terminal = Terminal::new(TestBackend::new(24, 8)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .expect("render");
    }

    #[test]
    fn renders_focused_query_pane_with_content() {
        use crate::config::Config;
        use edtui::Lines;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut browser = BrowserState::new();
        browser.sidebar.names = vec!["users".to_string()];
        browser.focus = Focus::Query;
        browser.query.state =
            EditorState::new(Lines::from("SELECT *\nFROM users\nWHERE age > 30"));

        let mut app = App {
            config: Config {
                connections: Vec::new(),
            },
            screen: Screen::Browser,
            worker: None,
            connection_name: "local".to_string(),
            catalog: Catalog::default(),
            browser,
            status: StatusLine::default(),
            pending: None,
            spinner_frame: 0,
            should_quit: false,
            select_seq: 0,
            latest_select_id: 0,
        };

        let mut terminal = Terminal::new(TestBackend::new(60, 20)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .expect("render");
    }

    #[test]
    fn renders_open_finder_overlay() {
        use crate::app::finder::FinderState;
        use crate::config::Config;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let names: Vec<String> = ["users", "products", "user_roles"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut browser = BrowserState::new();
        browser.sidebar.names = names.clone();
        let mut finder = FinderState::new(names);
        finder.input = TextInput::with_text("usr");
        finder.recompute();
        browser.focus = Focus::TableFinder(finder);

        let mut app = App {
            config: Config {
                connections: Vec::new(),
            },
            screen: Screen::Browser,
            worker: None,
            connection_name: "local".to_string(),
            catalog: Catalog::default(),
            browser,
            status: StatusLine::default(),
            pending: None,
            spinner_frame: 0,
            should_quit: false,
            select_seq: 0,
            latest_select_id: 0,
        };

        let mut terminal = Terminal::new(TestBackend::new(50, 16)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .expect("render");
    }

    #[test]
    fn no_primary_key_falls_back_to_full_row_match() {
        let mut grid = GridView::new(vec![col("name", false), col("email", false)], false);
        grid.set_rows(vec![vec![
            Value::Text("John".to_string()),
            Value::Text("j@x".to_string()),
        ]]);
        grid.record_edit(0, 1, Value::Text("j2@x".to_string()));

        let mutations = grid.build_mutations("people");
        assert_eq!(mutations.len(), 1);
        assert_eq!(
            mutations[0].key,
            RowKey::FullRow(vec![
                KeyPart {
                    column: "name".to_string(),
                    value: Value::Text("John".to_string()),
                },
                KeyPart {
                    column: "email".to_string(),
                    value: Value::Text("j@x".to_string()),
                },
            ])
        );
    }
}
