//! Context-aware autocompletion for the SQL query pane.
//!
//! The popup is summoned automatically when writing and then refined live as the user
//! types. Candidates are drawn from the harvested [`Catalog`] plus a static set
//! of SQL keywords, and which kinds are offered depends on where the cursor is:
//!
//! * Right after `FROM`/`JOIN` (and friends) we are naming a relation, so only
//!   table names (and keywords) are suggested.
//! * Anywhere else we are in a column position. If the query already references
//!   one or more known tables, only those tables' columns are offered; if no
//!   known table is referenced yet, we fall back to "everything" (all columns
//!   and all tables), per the requested behaviour.

use edtui::{EditorState, Index2};

use crate::app::editor::TextInput;
use crate::app::finder::fuzzy_rank;
use crate::model::schema::Catalog;

/// A static set of common SQL keywords offered as completions.
const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "ON",
    "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "OFFSET", "AS", "AND", "OR", "NOT", "NULL", "IS",
    "IN", "LIKE", "BETWEEN", "DISTINCT", "COUNT", "SUM", "AVG", "MIN", "MAX", "INSERT", "INTO",
    "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "DROP", "ALTER", "UNION", "ALL",
    "EXISTS", "CASE", "WHEN", "THEN", "ELSE", "END", "ASC", "DESC",
];

/// Keywords after which the cursor is naming a relation rather than a column.
const TABLE_POSITION_KEYWORDS: &[&str] = &["FROM", "JOIN", "INTO", "UPDATE", "TABLE"];

/// What kind of symbol a candidate is, used for the display tag and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Table,
    Column,
    Keyword,
}

/// One ranked completion candidate.
#[derive(Debug, Clone)]
pub struct CompletionItem {
    pub text: String,
    pub kind: CandidateKind,
}

/// An open completion popup over the query editor.
pub struct Completion {
    /// The char column on the cursor's row where the typed prefix begins. The
    /// region `[start_col, cursor.col)` is replaced when a candidate is taken.
    start_col: usize,
    items: Vec<CompletionItem>,
    selected: usize,
}

impl Completion {
    /// Build the candidate list for the editor's current cursor position. The
    /// result may be empty (e.g. a prefix that matches nothing); callers should
    /// treat an empty completion as "do not open the popup".
    pub fn build(state: &EditorState, catalog: &Catalog) -> Completion {
        let full = state.lines.to_string();
        let lines: Vec<&str> = full.split('\n').collect();
        let row = state.cursor.row;
        let line_chars: Vec<char> = lines.get(row).copied().unwrap_or("").chars().collect();
        let col = state.cursor.col.min(line_chars.len());

        // Walk back over identifier characters to find the prefix being typed.
        let mut start_col = col;
        while start_col > 0 && is_ident_char(line_chars[start_col - 1]) {
            start_col -= 1;
        }
        let prefix: String = line_chars[start_col..col].iter().collect();

        let before = text_before_cursor(&lines, row, start_col);
        let position = position_at(&before);
        let referenced = referenced_tables(&full, catalog);
        let candidates = assemble(position, &referenced, catalog);

        let names: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();
        let ranked = fuzzy_rank(&names, &prefix);
        let items: Vec<CompletionItem> = ranked
            .into_iter()
            .filter_map(|i| candidates.get(i).cloned())
            .collect();

        Completion {
            start_col,
            items,
            selected: 0,
        }
    }

    /// Build candidates for a single-line filter/order field. Such a field is
    /// always a column position scoped to the loaded `table` (if any known); when
    /// no known table is loaded it falls back to every column and table.
    pub fn for_field(
        text: &str,
        cursor: usize,
        catalog: &Catalog,
        table: Option<&str>,
    ) -> Completion {
        let chars: Vec<char> = text.chars().collect();
        let cursor = cursor.min(chars.len());
        let mut start_col = cursor;
        while start_col > 0 && is_ident_char(chars[start_col - 1]) {
            start_col -= 1;
        }
        let prefix: String = chars[start_col..cursor].iter().collect();

        let referenced: Vec<String> = match table {
            Some(name) if catalog.find(name).is_some() => vec![name.to_string()],
            _ => Vec::new(),
        };
        let candidates = assemble(Position::Column, &referenced, catalog);

        let names: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();
        let ranked = fuzzy_rank(&names, &prefix);
        let items: Vec<CompletionItem> = ranked
            .into_iter()
            .filter_map(|i| candidates.get(i).cloned())
            .collect();

        Completion {
            start_col,
            items,
            selected: 0,
        }
    }

    pub fn items(&self) -> &[CompletionItem] {
        &self.items
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() - 1;
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, last as isize) as usize;
    }

    fn selected_item(&self) -> Option<&CompletionItem> {
        self.items.get(self.selected)
    }
}

/// Replace the typed prefix in `state` with the currently selected candidate,
/// leaving the cursor at the end of the inserted text.
pub fn accept(state: &mut EditorState, completion: &Completion) {
    let Some(item) = completion.selected_item() else {
        return;
    };
    let row = state.cursor.row;
    let start = completion.start_col;
    let end = state.cursor.col;

    // Remove the prefix one char at a time; each removal shifts the rest left,
    // so repeatedly removing at `start` deletes the whole `[start, end)` span.
    for _ in start..end {
        if state.lines.get(Index2::new(row, start)).is_some() {
            state.lines.remove(Index2::new(row, start));
        }
    }
    for (i, ch) in item.text.chars().enumerate() {
        state.lines.insert(Index2::new(row, start + i), ch);
    }
    state.cursor.col = start + item.text.chars().count();
}

/// Replace the typed prefix in a single-line `input` with the selected candidate.
pub fn accept_field(input: &mut TextInput, completion: &Completion) {
    if let Some(item) = completion.selected_item() {
        input.replace_prefix(completion.start_col, &item.text);
    }
}

/// Whether a character is part of an identifier (and thus part of a prefix).
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier word immediately before `cursor` in `text` (an empty string if
/// the cursor isn't at the end of a word). Used to decide whether to auto-open
/// the popup in a single-line field.
pub fn field_prefix(text: &str, cursor: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut start = cursor;
    while start > 0 && is_ident_char(chars[start - 1]) {
        start -= 1;
    }
    chars[start..cursor].iter().collect()
}

/// The identifier word immediately before the editor's cursor. Used to decide
/// whether to auto-open the popup in the query pane.
pub fn editor_prefix(state: &EditorState) -> String {
    let full = state.lines.to_string();
    let line = full.split('\n').nth(state.cursor.row).unwrap_or("");
    field_prefix(line, state.cursor.col)
}

/// The text of the buffer up to (but excluding) the prefix being completed.
fn text_before_cursor(lines: &[&str], row: usize, start_col: usize) -> String {
    let mut out = String::new();
    for line in lines.iter().take(row) {
        out.push_str(line);
        out.push('\n');
    }
    if let Some(line) = lines.get(row) {
        out.extend(line.chars().take(start_col));
    }
    out
}

/// Whether the cursor is naming a relation or a column, decided by the most
/// recent positioning keyword before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    Table,
    Column,
}

fn position_at(before: &str) -> Position {
    match last_keyword(before) {
        Some(kw) if TABLE_POSITION_KEYWORDS.contains(&kw.as_str()) => Position::Table,
        _ => Position::Column,
    }
}

/// The last word in `text` that is a known SQL keyword (upper-cased), if any.
fn last_keyword(text: &str) -> Option<String> {
    words(text)
        .into_iter()
        .rev()
        .map(|w| w.to_uppercase())
        .find(|w| KEYWORDS.contains(&w.as_str()))
}

/// Split `text` into identifier words, dropping all other characters.
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if is_ident_char(c) {
            current.push(c);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Catalog tables referenced after a `FROM`/`JOIN` in the full query text.
fn referenced_tables(full: &str, catalog: &Catalog) -> Vec<String> {
    let words = words(full);
    let mut tables = Vec::new();
    for pair in words.windows(2) {
        let keyword = pair[0].to_uppercase();
        if (keyword == "FROM" || keyword == "JOIN")
            && let Some(table) = catalog.find(&pair[1])
            && !tables.contains(&table.name)
        {
            tables.push(table.name.clone());
        }
    }
    tables
}

/// Build the (unranked) candidate set for a cursor position.
fn assemble(position: Position, referenced: &[String], catalog: &Catalog) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    match position {
        Position::Table => {
            push_tables(&mut items, catalog);
            push_keywords(&mut items);
        }
        Position::Column => {
            if referenced.is_empty() {
                // No known table in scope yet: offer everything.
                push_columns(&mut items, catalog, None);
                push_tables(&mut items, catalog);
            } else {
                push_columns(&mut items, catalog, Some(referenced));
            }
            push_keywords(&mut items);
        }
    }
    items
}

fn push_tables(items: &mut Vec<CompletionItem>, catalog: &Catalog) {
    for table in &catalog.tables {
        items.push(CompletionItem {
            text: table.name.clone(),
            kind: CandidateKind::Table,
        });
    }
}

/// Push column candidates, de-duplicated by name. When `only` is `Some`, restrict
/// to those tables; otherwise include every table's columns.
fn push_columns(items: &mut Vec<CompletionItem>, catalog: &Catalog, only: Option<&[String]>) {
    let mut seen = std::collections::HashSet::new();
    for table in &catalog.tables {
        if let Some(names) = only
            && !names.iter().any(|n| n == &table.name)
        {
            continue;
        }
        for column in &table.columns {
            if seen.insert(column.name.clone()) {
                items.push(CompletionItem {
                    text: column.name.clone(),
                    kind: CandidateKind::Column,
                });
            }
        }
    }
}

fn push_keywords(items: &mut Vec<CompletionItem>) {
    for keyword in KEYWORDS {
        items.push(CompletionItem {
            text: (*keyword).to_string(),
            kind: CandidateKind::Keyword,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::schema::{ColumnMeta, RelationKind, TableMeta};
    use crate::model::value::TypeAffinity;
    use edtui::Lines;

    fn col(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.to_string(),
            declared_type: "text".to_string(),
            affinity: TypeAffinity::Text,
            is_primary_key: false,
        }
    }

    fn catalog() -> Catalog {
        Catalog {
            tables: vec![
                TableMeta {
                    name: "users".to_string(),
                    kind: RelationKind::Table,
                    columns: vec![col("id"), col("email"), col("name")],
                },
                TableMeta {
                    name: "orders".to_string(),
                    kind: RelationKind::Table,
                    columns: vec![col("id"), col("total"), col("user_id")],
                },
            ],
        }
    }

    /// Build an editor whose cursor sits at the end of `text`.
    fn editor_at_end(text: &str) -> EditorState {
        let mut state = EditorState::new(Lines::from(text));
        let lines: Vec<&str> = text.split('\n').collect();
        let row = lines.len().saturating_sub(1);
        let col = lines.last().map(|l| l.chars().count()).unwrap_or(0);
        state.cursor = Index2::new(row, col);
        state
    }

    fn kinds(c: &Completion, kind: CandidateKind) -> Vec<String> {
        c.items()
            .iter()
            .filter(|i| i.kind == kind)
            .map(|i| i.text.clone())
            .collect()
    }

    #[test]
    fn after_from_suggests_tables_only() {
        let state = editor_at_end("SELECT * FROM us");
        let c = Completion::build(&state, &catalog());
        let tables = kinds(&c, CandidateKind::Table);
        assert!(tables.contains(&"users".to_string()));
        // "us" fuzzy-matches no column, so the column kind should be absent.
        assert!(kinds(&c, CandidateKind::Column).is_empty());
    }

    #[test]
    fn columns_scoped_to_referenced_table() {
        let state = editor_at_end("SELECT  FROM orders");
        // Place the cursor right after "SELECT " (a column position).
        let mut state = state;
        state.cursor = Index2::new(0, 7);
        let c = Completion::build(&state, &catalog());
        let columns = kinds(&c, CandidateKind::Column);
        // Only `orders` columns, not `users`-only columns like `email`.
        assert!(columns.contains(&"total".to_string()));
        assert!(columns.contains(&"user_id".to_string()));
        assert!(!columns.contains(&"email".to_string()));
        assert!(!columns.contains(&"name".to_string()));
    }

    #[test]
    fn no_referenced_table_offers_all_columns_and_tables() {
        let state = editor_at_end("SELECT em");
        let c = Completion::build(&state, &catalog());
        // "em" matches the `email` column even with no FROM clause yet.
        assert!(kinds(&c, CandidateKind::Column).contains(&"email".to_string()));
    }

    #[test]
    fn field_completion_scopes_to_loaded_table() {
        // A WHERE-fragment field for `orders` should only offer its columns.
        let c = Completion::for_field("us", 2, &catalog(), Some("orders"));
        let columns = kinds(&c, CandidateKind::Column);
        assert!(columns.contains(&"user_id".to_string()));
        assert!(!columns.contains(&"email".to_string()));
    }

    #[test]
    fn accept_field_replaces_prefix_mid_string() {
        let mut input = TextInput::with_text("tot");
        // Cursor sits at the end of "tot".
        let c = Completion::for_field("tot", 3, &catalog(), Some("orders"));
        accept_field(&mut input, &c);
        assert_eq!(input.text(), "total");
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn field_prefix_reads_word_under_cursor() {
        assert_eq!(field_prefix("age > us", 8), "us");
        assert_eq!(field_prefix("age > ", 6), "");
    }

    #[test]
    fn accept_replaces_prefix_in_place() {
        let mut state = editor_at_end("SELECT * FROM us");
        let c = Completion::build(&state, &catalog());
        // Top match for "us" should be `users`.
        accept(&mut state, &c);
        assert_eq!(state.lines.to_string(), "SELECT * FROM users");
        assert_eq!(state.cursor.col, "SELECT * FROM users".chars().count());
    }
}
