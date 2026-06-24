//! Lightweight single-line text editing primitives used by the filter/order
//! boxes and by in-place cell editing.

/// A single-line editable text buffer with a cursor.
///
/// Characters are stored as a `Vec<char>` so cursor arithmetic is in terms of
/// grapheme-ish units rather than raw bytes, which keeps multi-byte input from
/// corrupting the buffer.
#[derive(Debug, Clone, Default)]
pub struct TextInput {
    chars: Vec<char>,
    cursor: usize,
}

impl TextInput {
    /// Seed the buffer with existing text and place the cursor at the end.
    pub fn with_text(text: &str) -> TextInput {
        let chars: Vec<char> = text.chars().collect();
        let cursor = chars.len();
        TextInput { chars, cursor }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Cursor position as a character offset from the start.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    /// Delete the word before the cursor (readline `Ctrl+W`): first any run of
    /// whitespace, then the run of non-whitespace before it.
    pub fn delete_word(&mut self) {
        while self.cursor > 0 && self.chars[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
        while self.cursor > 0 && !self.chars[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    /// Delete from the cursor back to the start of the line (readline `Ctrl+U`).
    pub fn delete_to_start(&mut self) {
        self.chars.drain(0..self.cursor);
        self.cursor = 0;
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Replace the half-open char range `[start, cursor)` with `text`, leaving
    /// the cursor at the end of the inserted text. Used to apply a completion in
    /// place of the prefix the user had typed.
    pub fn replace_prefix(&mut self, start: usize, text: &str) {
        if start > self.cursor || self.cursor > self.chars.len() {
            return;
        }
        self.chars.drain(start..self.cursor);
        let inserted: Vec<char> = text.chars().collect();
        let count = inserted.len();
        for (offset, ch) in inserted.into_iter().enumerate() {
            self.chars.insert(start + offset, ch);
        }
        self.cursor = start + count;
    }
}

/// An in-place cell editor: the buffer plus the grid coordinates it edits.
///
/// Holding the coordinates inside the editor means the application can only be
/// in "editing a cell" when it actually has a target cell — the type system
/// rules out an editing state without a location.
#[derive(Debug, Clone)]
pub struct CellEditor {
    pub row: usize,
    pub col: usize,
    pub input: TextInput,
}

impl CellEditor {
    pub fn new(row: usize, col: usize, seed: &str) -> CellEditor {
        CellEditor {
            row,
            col,
            input: TextInput::with_text(seed),
        }
    }
}
