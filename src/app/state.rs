//! Application state and the top-level state machine.

use std::collections::{HashMap, HashSet};

use edtui::{EditorEventHandler, EditorState, Lines};

use crate::app::completion::Completion;
use crate::app::conn_form::{ConnectionDraft, TestStatus};
use crate::app::editor::{CellEditor, TextInput};
use crate::app::finder::FinderState;
use crate::app::inspect::InspectState;
use crate::app::picker::{PickerPrompt, PickerState};
use crate::clipboard::ClipboardSink;
use crate::config::Config;
use crate::db::query::SelectQuery;
use crate::model::delta::{CellDelta, KeyPart, RowKey, RowMutation};
use crate::model::schema::{Catalog, ColumnMeta};
use crate::model::value::{TypeAffinity, Value};
use crate::worker::{TestHandle, TestOutcome, WorkerHandle, WorkerRequest, WorkerResponse};

/// Default row cap for browse queries.
pub const ROW_LIMIT: u32 = 100;

/// The operation currently in flight on the worker, for the spinner/status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOp {
    Schema,
    SchemaRefresh,
    Select,
    Commit,
}

impl PendingOp {
    pub fn label(&self) -> &'static str {
        match self {
            PendingOp::Schema => "Loading schema",
            PendingOp::SchemaRefresh => "Refreshing schema",
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
    /// Choosing among the configured connections (fuzzy-filterable).
    Picker(PickerState),
    /// Creating or editing a connection.
    ConnectionForm(ConnectionDraft),
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
    Controls {
        field: ControlsField,
        input: TextInput,
    },
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
    /// The read-only cell/row inspector overlay (summoned from the data grid).
    Inspect(InspectState),
}

impl Focus {
    /// The pane number this focus belongs to (for highlighting).
    pub fn pane(&self) -> u8 {
        match self {
            Focus::Controls { .. } => 1,
            // The finder overlays the catalog.
            Focus::Catalog | Focus::TableFinder(_) => 2,
            Focus::Query => 3,
            Focus::Data | Focus::CellEdit(_) | Focus::Inspect(_) => 4,
        }
    }
}

/// The catalog sidebar.
#[derive(Debug, Default)]
pub struct SidebarState {
    pub names: Vec<String>,
    pub selected: usize,
}

impl SidebarState {
    /// Move the selection by `delta` entries, clamped to the list bounds.
    pub fn move_selection(&mut self, delta: isize) {
        if self.names.is_empty() {
            return;
        }
        let last = self.names.len() - 1;
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, last as isize) as usize;
    }
}

/// The virtualized data grid: original rows plus an overlay of pending edits.
pub struct GridView {
    pub columns: Vec<ColumnMeta>,
    /// Original values as returned by the last query.
    pub rows: Vec<Vec<Value>>,
    /// Pending edits keyed by `(row, col)`; the source of truth for what is
    /// displayed (amber) and what will be committed.
    pub overlay: HashMap<(usize, usize), Value>,
    /// Rows marked for deletion (displayed red); committed as `DELETE`s in the
    /// same transaction as the edits.
    pub pending_deletes: HashSet<usize>,
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
            pending_deletes: HashSet::new(),
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
            pending_deletes: HashSet::new(),
            sel_row: 0,
            sel_col: 0,
            read_only,
        }
    }

    pub fn set_rows(&mut self, rows: Vec<Vec<Value>>) {
        self.rows = rows.into_iter().map(|row| self.type_row(row)).collect();
        self.overlay.clear();
        self.pending_deletes.clear();
        self.clamp_selection();
    }

    /// Re-type a freshly-decoded row against the column affinities. Engines
    /// decode JSON columns as text (Postgres casts every column to text;
    /// MySQL/SQLite have no distinct JSON value), so the JSON shape is recovered
    /// here from the schema.
    fn type_row(&self, row: Vec<Value>) -> Vec<Value> {
        row.into_iter()
            .enumerate()
            .map(|(col, value)| match self.columns.get(col) {
                Some(meta) if meta.affinity == TypeAffinity::Json => match value {
                    Value::Text(s) => Value::Json(s),
                    other => other,
                },
                _ => value,
            })
            .collect()
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

    pub fn is_pending_delete(&self, row: usize) -> bool {
        self.pending_deletes.contains(&row)
    }

    /// Toggle the pending-deletion mark on `row`. Marking a row drops any pending
    /// cell edits for it: the row is going away, so those edits are moot.
    pub fn toggle_delete(&mut self, row: usize) {
        if self.pending_deletes.remove(&row) {
            return;
        }
        self.pending_deletes.insert(row);
        self.overlay.retain(|&(r, _), _| r != row);
    }

    /// Total pending row operations (edited cells plus marked-for-deletion rows).
    pub fn pending_count(&self) -> usize {
        self.overlay.len() + self.pending_deletes.len()
    }

    pub fn has_pending(&self) -> bool {
        !self.overlay.is_empty() || !self.pending_deletes.is_empty()
    }

    /// Serialize a row to a compact JSON object using the displayed values
    /// (pending edits included). Keys preserve column order. Returns `None` if
    /// the row index is out of range or serialization fails.
    pub fn row_json(&self, row: usize) -> Option<String> {
        if row >= self.row_count() {
            return None;
        }
        // `preserve_order` keeps the map in column order.
        let mut object = serde_json::Map::with_capacity(self.columns.len());
        for (col, column) in self.columns.iter().enumerate() {
            let value = self.display_value(row, col).cloned().unwrap_or(Value::Null);
            object.insert(column.name.clone(), value.to_json());
        }
        serde_json::to_string(&serde_json::Value::Object(object)).ok()
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

    /// Compile the pending state into one [`RowMutation`] per affected row: a
    /// `Delete` for each marked row and an `Update` for each edited row. A row
    /// marked for deletion carries no overlay edits (see [`Self::toggle_delete`]),
    /// so the two sets never overlap.
    pub fn build_mutations(&self, table: &str) -> Vec<RowMutation> {
        let mut mutations: Vec<RowMutation> = Vec::new();

        for &row in &self.pending_deletes {
            let Some(row_values) = self.rows.get(row) else {
                continue;
            };
            mutations.push(RowMutation::Delete {
                table: table.to_string(),
                key: self.row_key(row_values),
            });
        }

        // Group overlay entries by row.
        let mut by_row: HashMap<usize, Vec<usize>> = HashMap::new();
        for (row, col) in self.overlay.keys() {
            by_row.entry(*row).or_default().push(*col);
        }

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
                mutations.push(RowMutation::Update {
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
    /// The open autocompletion popup, if any. Shared by the query pane and the
    /// controls fields; only one text context is focused at a time.
    pub completion: Option<Completion>,
    /// Visible body-row count of the data grid from the last render, used to size
    /// half-page (`Ctrl+U`/`Ctrl+D`) scrolling.
    pub grid_viewport_rows: usize,
    /// Visible row count of the catalog list from the last render.
    pub catalog_viewport_rows: usize,
    /// Visible body-line count of the inspector overlay from the last render,
    /// used to size half-page scrolling.
    pub inspect_viewport_rows: usize,
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
            completion: None,
            grid_viewport_rows: 0,
            catalog_viewport_rows: 0,
            inspect_viewport_rows: 0,
        }
    }
}

/// The whole application.
pub struct App {
    pub config: Config,
    /// Where the connections config is read from and written back to.
    pub config_path: String,
    pub screen: Screen,
    pub worker: Option<WorkerHandle>,
    pub connection_name: String,
    pub catalog: Catalog,
    pub browser: BrowserState,
    pub status: StatusLine,
    pub pending: Option<PendingOp>,
    pub spinner_frame: usize,
    pub should_quit: bool,
    /// System clipboard sink for yanking (degrades to a no-op if unavailable).
    pub clipboard: ClipboardSink,
    /// The most recent yanked text, kept as an in-app register/fallback.
    pub last_yank: Option<String>,
    select_seq: u64,
    latest_select_id: u64,
}

impl App {
    /// Build the app for `config`. A single-connection config connects
    /// immediately; multiple connections open the picker.
    pub fn new(config: Config, config_path: String) -> App {
        let picker = PickerState::new(&config);
        let mut app = App {
            config,
            config_path,
            screen: Screen::Picker(picker),
            worker: None,
            connection_name: String::new(),
            catalog: Catalog::default(),
            browser: BrowserState::new(),
            status: StatusLine::default(),
            pending: None,
            spinner_frame: 0,
            should_quit: false,
            clipboard: ClipboardSink::new(),
            last_yank: None,
            select_seq: 0,
            latest_select_id: 0,
        };

        if app.config.connections.len() == 1 {
            app.start_connection(0);
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

    /// Open the connection picker, preserving the current session (if any) so the
    /// user can cancel back to it.
    pub fn open_picker(&mut self) {
        self.screen = Screen::Picker(PickerState::new(&self.config));
    }

    /// Connect to the picker's highlighted connection. When a session is live
    /// and the grid has uncommitted edits, the first call arms a confirmation
    /// (so the switch doesn't silently discard them); the second performs it.
    pub fn picker_connect(&mut self) {
        let (index, already_confirmed) = match &self.screen {
            Screen::Picker(picker) => match picker.selected_connection() {
                Some(index) => (
                    index,
                    picker.prompt == Some(PickerPrompt::ConfirmSwitch(index)),
                ),
                None => return,
            },
            _ => return,
        };
        let needs_confirm = self.worker.is_some() && self.browser.grid.has_pending();
        if needs_confirm && !already_confirmed {
            if let Screen::Picker(picker) = &mut self.screen {
                picker.prompt = Some(PickerPrompt::ConfirmSwitch(index));
            }
            return;
        }
        self.start_connection(index);
    }

    /// Open the create form for a new connection.
    pub fn picker_new(&mut self) {
        self.screen = Screen::ConnectionForm(ConnectionDraft::new());
    }

    /// Open the edit form pre-filled from the highlighted connection.
    pub fn picker_edit(&mut self) {
        let index = match &self.screen {
            Screen::Picker(picker) => picker.selected_connection(),
            _ => None,
        };
        let Some(index) = index else {
            return;
        };
        let Some(conn) = self.config.connections.get(index) else {
            return;
        };
        let draft = ConnectionDraft::from_existing(index, conn);
        self.screen = Screen::ConnectionForm(draft);
    }

    /// Delete the highlighted connection. The first call arms a confirmation;
    /// the second performs the delete and persists the config.
    pub fn picker_delete(&mut self) {
        let index = match &self.screen {
            Screen::Picker(picker) => picker.selected_connection(),
            _ => None,
        };
        let Some(index) = index else {
            return;
        };
        let armed = matches!(
            &self.screen,
            Screen::Picker(picker) if picker.prompt == Some(PickerPrompt::ConfirmDelete(index))
        );
        if !armed {
            if let Screen::Picker(picker) = &mut self.screen {
                picker.prompt = Some(PickerPrompt::ConfirmDelete(index));
            }
            return;
        }
        self.config.remove(index);
        if let Err(e) = self.config.save(&self.config_path) {
            self.error(format!("Could not save config: {e}"));
        }
        self.screen = Screen::Picker(PickerState::new(&self.config));
    }

    /// Validate and persist the form's draft, returning to the picker on success.
    /// Validation or write failures are surfaced on the form and keep it open.
    pub fn form_save(&mut self) {
        let result = match &self.screen {
            Screen::ConnectionForm(draft) => draft.validate().map(|conn| (draft.editing, conn)),
            _ => return,
        };
        let (editing, connection) = match result {
            Ok(pair) => pair,
            Err(err) => {
                self.set_form_test(TestStatus::Failed(err.message().to_string()));
                return;
            }
        };
        self.config.upsert(editing, connection);
        if let Err(e) = self.config.save(&self.config_path) {
            self.set_form_test(TestStatus::Failed(format!("Save failed: {e}")));
            return;
        }
        self.screen = Screen::Picker(PickerState::new(&self.config));
    }

    /// Start an ephemeral connection test for the form's draft. The attempt runs
    /// on a throwaway thread so the UI never blocks; [`Self::poll_test`] applies
    /// the outcome.
    pub fn form_test(&mut self) {
        let config = match &self.screen {
            Screen::ConnectionForm(draft) => match draft.validate() {
                Ok(conn) => conn.connection,
                Err(err) => {
                    self.set_form_test(TestStatus::Failed(err.message().to_string()));
                    return;
                }
            },
            _ => return,
        };
        self.set_form_test(TestStatus::Testing(TestHandle::spawn(config)));
    }

    /// Close the form without saving, returning to the picker.
    pub fn form_cancel(&mut self) {
        self.screen = Screen::Picker(PickerState::new(&self.config));
    }

    fn set_form_test(&mut self, status: TestStatus) {
        if let Screen::ConnectionForm(draft) = &mut self.screen {
            draft.test = status;
        }
    }

    /// Poll the in-flight connection test, applying its outcome to the form once
    /// it resolves. The handle lives in [`TestStatus::Testing`], so this is a
    /// no-op unless a test is actually running.
    pub fn poll_test(&mut self) {
        let Screen::ConnectionForm(draft) = &mut self.screen else {
            return;
        };
        let TestStatus::Testing(handle) = &draft.test else {
            return;
        };
        let Some(outcome) = handle.try_recv() else {
            return;
        };
        draft.test = match outcome {
            TestOutcome::Ok => TestStatus::Ok,
            TestOutcome::Failed(msg) => TestStatus::Failed(msg),
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
            WorkerResponse::Schema(catalog) => match self.pending {
                Some(PendingOp::SchemaRefresh) => self.on_schema_refresh(catalog),
                _ => self.on_schema(catalog),
            },
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

    /// Re-harvest the schema from the database while keeping the current view.
    /// Reuses the same worker request as the initial connect, but the response
    /// is applied non-destructively (see [`Self::on_schema_refresh`]).
    pub fn refresh_schema(&mut self) {
        self.send(WorkerRequest::HarvestSchema);
        self.pending = Some(PendingOp::SchemaRefresh);
        self.info("Refreshing schema...");
    }

    /// Apply a refreshed catalog without disturbing what the user is looking at:
    /// the loaded table, grid (and its pending edits), query and focus are all
    /// preserved. Only the catalog (used by completion) and the sidebar list are
    /// updated, keeping the highlight on the same relation when it still exists.
    fn on_schema_refresh(&mut self, catalog: Catalog) {
        let selected_name = self
            .browser
            .sidebar
            .names
            .get(self.browser.sidebar.selected)
            .cloned();
        let names: Vec<String> = catalog.tables.iter().map(|t| t.name.clone()).collect();
        self.catalog = catalog;
        self.browser.sidebar.selected = selected_name
            .and_then(|name| names.iter().position(|n| *n == name))
            .unwrap_or(0);
        self.browser.sidebar.names = names;
        self.pending = None;
        self.info("Schema refreshed");
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
            self.info(format!(
                "{count} rows (capped at {})",
                crate::db::RAW_ROW_CAP
            ));
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

    /// Build the completion candidates for whichever text context is focused.
    /// Returns `None` if the focus isn't a completable text field.
    fn build_completion(&self) -> Option<Completion> {
        match &self.browser.focus {
            Focus::Query => Some(Completion::build(&self.browser.query.state, &self.catalog)),
            Focus::Controls { input, .. } => Some(Completion::for_field(
                &input.text(),
                input.cursor(),
                &self.catalog,
                self.browser.loaded_table.as_deref(),
            )),
            _ => None,
        }
    }

    /// Open the autocompletion popup for the focused text context. Does nothing
    /// if there is nothing to suggest.
    pub fn open_completion(&mut self) {
        self.browser.completion = self.build_completion().filter(|c| !c.is_empty());
    }

    /// Move the completion selection by `delta` (down is positive).
    pub fn move_completion(&mut self, delta: isize) {
        if let Some(completion) = &mut self.browser.completion {
            completion.move_selection(delta);
        }
    }

    /// Insert the selected completion into the focused buffer and close the popup.
    pub fn accept_completion(&mut self) {
        let Some(completion) = self.browser.completion.take() else {
            return;
        };
        match &mut self.browser.focus {
            Focus::Query => {
                crate::app::completion::accept(&mut self.browser.query.state, &completion);
            }
            Focus::Controls { input, .. } => {
                crate::app::completion::accept_field(input, &completion);
            }
            _ => {}
        }
    }

    /// Close the completion popup without inserting anything.
    pub fn cancel_completion(&mut self) {
        self.browser.completion = None;
    }

    /// The identifier prefix under the cursor of the focused text context, if
    /// the focus is completable.
    fn focused_prefix(&self) -> Option<String> {
        match &self.browser.focus {
            Focus::Query => Some(crate::app::completion::editor_prefix(
                &self.browser.query.state,
            )),
            Focus::Controls { input, .. } => Some(crate::app::completion::field_prefix(
                &input.text(),
                input.cursor(),
            )),
            _ => None,
        }
    }

    /// Auto-show/refresh the popup as the user types: show it while an identifier
    /// prefix is present under the cursor, hide it otherwise. Used by the query
    /// pane, which completes automatically.
    pub fn autocomplete(&mut self) {
        let Some(prefix) = self.focused_prefix() else {
            return;
        };
        self.browser.completion = if prefix.is_empty() {
            None
        } else {
            self.build_completion().filter(|c| !c.is_empty())
        };
    }

    /// Scroll the data grid by half a visible page; `dir` is +1 for down, -1 up.
    pub fn scroll_grid_half(&mut self, dir: isize) {
        let half = (self.browser.grid_viewport_rows / 2).max(1) as isize;
        self.browser.awaiting_g = false;
        self.browser.grid.move_row(dir * half);
    }

    /// Scroll the catalog list by half a visible page; `dir` is +1 down, -1 up.
    pub fn scroll_catalog_half(&mut self, dir: isize) {
        let half = (self.browser.catalog_viewport_rows / 2).max(1) as isize;
        self.browser.sidebar.move_selection(dir * half);
    }

    /// Recompute the popup after a buffer edit, closing it if nothing matches.
    pub fn refresh_completion(&mut self) {
        if self.browser.completion.is_none() {
            return;
        }
        self.browser.completion = self.build_completion().filter(|c| !c.is_empty());
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
        self.browser.completion = None;
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

    /// Yank the selected cell's displayed value to the clipboard/register.
    pub fn yank_cell(&mut self) {
        let grid = &self.browser.grid;
        if grid.row_count() == 0 || grid.col_count() == 0 {
            self.info("Nothing to yank");
            return;
        }
        let text = grid
            .display_value(grid.sel_row, grid.sel_col)
            .map(|v| v.to_string())
            .unwrap_or_default();
        self.yank(text, "cell");
    }

    /// Yank the selected row as a JSON object to the clipboard/register.
    pub fn yank_row(&mut self) {
        let grid = &self.browser.grid;
        if grid.row_count() == 0 || grid.col_count() == 0 {
            self.info("Nothing to yank");
            return;
        }
        match grid.row_json(grid.sel_row) {
            Some(json) => self.yank(json, "row (json)"),
            None => self.error("Could not serialize row"),
        }
    }

    /// Common yank path: record the value and best-effort copy to the clipboard.
    fn yank(&mut self, text: String, label: &str) {
        let copied = self.clipboard.copy(&text);
        let preview = yank_preview(&text);
        self.last_yank = Some(text);
        if copied {
            self.info(format!("Yanked {label} → clipboard: {preview}"));
        } else {
            self.info(format!("Yanked {label} (no clipboard): {preview}"));
        }
    }

    /// Open the inspector on the selected cell. Always available, even on
    /// read-only relations: inspection never mutates the grid.
    pub fn inspect_cell(&mut self) {
        let grid = &self.browser.grid;
        if grid.row_count() == 0 || grid.col_count() == 0 {
            self.info("Nothing to inspect");
            return;
        }
        let column = grid
            .columns
            .get(grid.sel_col)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let value = grid
            .display_value(grid.sel_row, grid.sel_col)
            .cloned()
            .unwrap_or(Value::Null);
        self.browser.focus = Focus::Inspect(InspectState::cell(column, value));
    }

    /// Open the inspector on the selected row (every column/value pair).
    pub fn inspect_row(&mut self) {
        let grid = &self.browser.grid;
        if grid.row_count() == 0 || grid.col_count() == 0 {
            self.info("Nothing to inspect");
            return;
        }
        let row = grid.sel_row;
        let fields = grid
            .columns
            .iter()
            .enumerate()
            .map(|(col, meta)| {
                let value = grid.display_value(row, col).cloned().unwrap_or(Value::Null);
                (meta.name.clone(), value)
            })
            .collect();
        self.browser.focus = Focus::Inspect(InspectState::row(fields));
    }

    /// Close the inspector, returning focus to the data grid.
    pub fn close_inspect(&mut self) {
        self.browser.focus = Focus::Data;
    }

    /// Scroll the inspector by `delta` display lines.
    pub fn scroll_inspect(&mut self, delta: isize) {
        if let Focus::Inspect(inspect) = &mut self.browser.focus {
            inspect.scroll_by(delta);
        }
    }

    /// Scroll the inspector by half a visible page; `dir` is +1 down, -1 up.
    pub fn scroll_inspect_half(&mut self, dir: isize) {
        let half = (self.browser.inspect_viewport_rows / 2).max(1) as isize;
        self.scroll_inspect(dir * half);
    }

    /// Toggle the pending deletion of the selected row. Deletions are committed
    /// alongside edits in the same transaction (see [`Self::commit_pending`]).
    pub fn delete_row(&mut self) {
        if self.browser.grid.read_only {
            self.error("This relation is read-only");
            return;
        }
        if self.browser.grid.row_count() == 0 || self.browser.grid.col_count() == 0 {
            self.info("Nothing to delete");
            return;
        }
        let row = self.browser.grid.sel_row;
        let was_marked = self.browser.grid.is_pending_delete(row);
        self.browser.grid.toggle_delete(row);
        if was_marked {
            self.info("Deletion unmarked");
        } else {
            self.info("Row marked for deletion (Ctrl+S commit / u discard)");
        }
    }

    /// Discard all pending edits and deletions.
    pub fn discard_pending(&mut self) {
        if self.browser.grid.has_pending() {
            self.browser.grid.overlay.clear();
            self.browser.grid.pending_deletes.clear();
            self.info("Pending changes discarded");
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

/// A short, single-line preview of yanked text for the status line.
fn yank_preview(text: &str) -> String {
    let one_line: String = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    if one_line.chars().count() > 40 {
        let head: String = one_line.chars().take(39).collect();
        format!("{head}…")
    } else {
        one_line
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
    fn sidebar_move_selection_clamps_to_bounds() {
        let mut sidebar = SidebarState {
            names: vec!["a".into(), "b".into(), "c".into()],
            selected: 0,
        };
        sidebar.move_selection(10); // half-page jump past the end
        assert_eq!(sidebar.selected, 2);
        sidebar.move_selection(-10);
        assert_eq!(sidebar.selected, 0);

        let mut empty = SidebarState::default();
        empty.move_selection(5); // must not panic on an empty list
        assert_eq!(empty.selected, 0);
    }

    #[test]
    fn edit_then_build_mutation_uses_primary_key() {
        let mut grid = grid_with_pk();
        grid.record_edit(0, 1, Value::Text("new@x".to_string()));
        assert!(grid.is_dirty(0, 1));

        let mutations = grid.build_mutations("users");
        assert_eq!(mutations.len(), 1);
        let RowMutation::Update {
            table,
            key,
            changes,
        } = &mutations[0]
        else {
            panic!("expected an update mutation");
        };
        assert_eq!(table, "users");
        assert_eq!(
            *key,
            RowKey::PrimaryKey(vec![KeyPart {
                column: "id".to_string(),
                value: Value::Integer(1),
            }])
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].original, Value::Text("a@x".to_string()));
        assert_eq!(changes[0].new, Value::Text("new@x".to_string()));
    }

    #[test]
    fn marking_a_row_for_deletion_builds_a_delete_and_drops_its_edits() {
        let mut grid = grid_with_pk();
        // An edit on the row that is about to be deleted must be discarded.
        grid.record_edit(0, 1, Value::Text("doomed@x".to_string()));
        grid.toggle_delete(0);
        assert!(grid.is_pending_delete(0));
        assert!(!grid.is_dirty(0, 1));

        let mutations = grid.build_mutations("users");
        assert_eq!(mutations.len(), 1);
        let RowMutation::Delete { table, key } = &mutations[0] else {
            panic!("expected a delete mutation");
        };
        assert_eq!(table, "users");
        assert_eq!(
            *key,
            RowKey::PrimaryKey(vec![KeyPart {
                column: "id".to_string(),
                value: Value::Integer(1),
            }])
        );

        // Toggling again clears the mark.
        grid.toggle_delete(0);
        assert!(!grid.is_pending_delete(0));
        assert!(grid.build_mutations("users").is_empty());
    }

    #[test]
    fn row_json_preserves_column_order_and_types() {
        let mut grid = GridView::new(
            vec![col("id", true), col("name", false), col("active", false)],
            false,
        );
        grid.set_rows(vec![vec![
            Value::Integer(7),
            Value::Text("Mara".to_string()),
            Value::Boolean(true),
        ]]);
        // A pending edit should be reflected in the yanked JSON.
        grid.record_edit(0, 1, Value::Null);

        let json = grid.row_json(0).expect("json");
        assert_eq!(json, r#"{"id":7,"name":null,"active":true}"#);
        assert!(grid.row_json(5).is_none());
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
    fn set_rows_retypes_json_columns_from_text() {
        let mut columns = vec![col("id", true), col("payload", false)];
        // Force the payload column to JSON affinity, as a real json column would.
        columns[1].affinity = TypeAffinity::Json;
        let mut grid = GridView::new(columns, true);
        grid.set_rows(vec![vec![
            Value::Integer(1),
            Value::Text(r#"{"k":1}"#.to_string()),
        ]]);

        // The id column stays an integer; the json column is re-typed.
        assert_eq!(grid.display_value(0, 0), Some(&Value::Integer(1)));
        assert_eq!(
            grid.display_value(0, 1),
            Some(&Value::Json(r#"{"k":1}"#.to_string()))
        );
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
            completion: None,
            grid_viewport_rows: 0,
            catalog_viewport_rows: 0,
            inspect_viewport_rows: 0,
        };

        let mut app = App {
            config: Config {
                connections: Vec::new(),
            },
            config_path: "test.toml".to_string(),
            screen: Screen::Browser,
            worker: None,
            connection_name: "local".to_string(),
            catalog: Catalog::default(),
            browser,
            status: StatusLine::default(),
            pending: Some(PendingOp::Select),
            spinner_frame: 3,
            should_quit: false,
            clipboard: ClipboardSink::disabled(),
            last_yank: None,
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
        browser.query.state = EditorState::new(Lines::from("SELECT *\nFROM users\nWHERE age > 30"));

        let mut app = App {
            config: Config {
                connections: Vec::new(),
            },
            config_path: "test.toml".to_string(),
            screen: Screen::Browser,
            worker: None,
            connection_name: "local".to_string(),
            catalog: Catalog::default(),
            browser,
            status: StatusLine::default(),
            pending: None,
            spinner_frame: 0,
            should_quit: false,
            clipboard: ClipboardSink::disabled(),
            last_yank: None,
            select_seq: 0,
            latest_select_id: 0,
        };

        let mut terminal = Terminal::new(TestBackend::new(60, 20)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .expect("render");
    }

    #[test]
    fn renders_query_pane_with_open_completion() {
        use crate::app::completion::Completion;
        use crate::config::Config;
        use crate::model::schema::{RelationKind, TableMeta};
        use edtui::{Index2, Lines};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let catalog = Catalog {
            tables: vec![TableMeta {
                name: "users".to_string(),
                kind: RelationKind::Table,
                columns: vec![col("id", true), col("email", false)],
            }],
        };

        let mut browser = BrowserState::new();
        browser.sidebar.names = vec!["users".to_string()];
        browser.focus = Focus::Query;
        browser.query.state = EditorState::new(Lines::from("SELECT * FROM us"));
        browser.query.state.cursor = Index2::new(0, 16);
        browser.completion = Some(Completion::build(&browser.query.state, &catalog));

        let mut app = App {
            config: Config {
                connections: Vec::new(),
            },
            config_path: "test.toml".to_string(),
            screen: Screen::Browser,
            worker: None,
            connection_name: "local".to_string(),
            catalog,
            browser,
            status: StatusLine::default(),
            pending: None,
            spinner_frame: 0,
            should_quit: false,
            clipboard: ClipboardSink::disabled(),
            last_yank: None,
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
            config_path: "test.toml".to_string(),
            screen: Screen::Browser,
            worker: None,
            connection_name: "local".to_string(),
            catalog: Catalog::default(),
            browser,
            status: StatusLine::default(),
            pending: None,
            spinner_frame: 0,
            should_quit: false,
            clipboard: ClipboardSink::disabled(),
            last_yank: None,
            select_seq: 0,
            latest_select_id: 0,
        };

        let mut terminal = Terminal::new(TestBackend::new(50, 16)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .expect("render");
    }

    #[test]
    fn inspect_row_snapshots_every_column_and_renders() {
        use crate::app::inspect::InspectTarget;
        use crate::config::Config;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut browser = BrowserState::new();
        browser.sidebar.names = vec!["users".to_string()];
        browser.loaded_table = Some("users".to_string());
        let mut grid = GridView::new(vec![col("id", true), col("email", false)], false);
        grid.set_rows(vec![vec![
            Value::Integer(1),
            // A value longer than the overlay width exercises wrapping/scrolling.
            Value::Text("a".repeat(120)),
        ]]);
        browser.grid = grid;

        let mut app = App {
            config: Config {
                connections: Vec::new(),
            },
            config_path: "test.toml".to_string(),
            screen: Screen::Browser,
            worker: None,
            connection_name: "local".to_string(),
            catalog: Catalog::default(),
            browser,
            status: StatusLine::default(),
            pending: None,
            spinner_frame: 0,
            should_quit: false,
            clipboard: ClipboardSink::disabled(),
            last_yank: None,
            select_seq: 0,
            latest_select_id: 0,
        };

        app.inspect_row();
        match &app.browser.focus {
            Focus::Inspect(inspect) => match &inspect.target {
                InspectTarget::Row { fields } => {
                    assert_eq!(fields.len(), 2);
                    assert_eq!(fields[0].0, "id");
                    assert_eq!(fields[1].0, "email");
                }
                InspectTarget::Cell { .. } => panic!("expected a row inspector"),
            },
            _ => panic!("expected the inspector to be focused"),
        }

        // A short viewport forces the scroll clamp; rendering must not panic, and
        // the scroll offset must settle within the wrapped content height.
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).expect("terminal");
        app.scroll_inspect(1000);
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .expect("render");
        if let Focus::Inspect(inspect) = &app.browser.focus {
            assert!(inspect.scroll < 1000);
        }
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
        let RowMutation::Update { key, .. } = &mutations[0] else {
            panic!("expected an update mutation");
        };
        assert_eq!(
            *key,
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

    fn bare_app(config: Config, screen: Screen) -> App {
        App {
            config,
            config_path: "test.toml".to_string(),
            screen,
            worker: None,
            connection_name: String::new(),
            catalog: Catalog::default(),
            browser: BrowserState::new(),
            status: StatusLine::default(),
            pending: None,
            spinner_frame: 0,
            should_quit: false,
            clipboard: ClipboardSink::disabled(),
            last_yank: None,
            select_seq: 0,
            latest_select_id: 0,
        }
    }

    #[test]
    fn renders_connection_picker_with_fuzzy_query() {
        use crate::app::picker::PickerState;
        use crate::config::{ConnectionConfig, NamedConnection};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let config = Config {
            connections: vec![
                NamedConnection {
                    name: "local".to_string(),
                    connection: ConnectionConfig::Sqlite {
                        path: "./a.db".to_string(),
                    },
                },
                NamedConnection {
                    name: "prod".to_string(),
                    connection: ConnectionConfig::Postgres {
                        url: "postgresql://u:p@h/db".to_string(),
                    },
                },
            ],
        };
        let mut picker = PickerState::new(&config);
        picker.finder.input = TextInput::with_text("pr");
        picker.finder.recompute();
        assert_eq!(picker.selected_connection(), Some(1));

        let mut app = bare_app(config, Screen::Picker(picker));
        let mut terminal = Terminal::new(TestBackend::new(50, 16)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .expect("render");
    }

    #[test]
    fn renders_connection_form_with_failed_test() {
        use crate::app::conn_form::{ConnectionDraft, DraftEngine, FormFocus};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut draft = ConnectionDraft::new();
        draft.engine = DraftEngine::Postgres;
        // Exercise the insert-mode (editing) render path on a text field.
        draft.focus = FormFocus::Value { editing: true };
        draft.name = TextInput::with_text("prod");
        draft.value = TextInput::with_text("postgresql://u:p@h/db");
        draft.test = TestStatus::Failed("connection refused".to_string());

        let mut app = bare_app(Config::default(), Screen::ConnectionForm(draft));
        // A cramped viewport stresses the wrapped test-status line.
        let mut terminal = Terminal::new(TestBackend::new(34, 12)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .expect("render");
    }
}
