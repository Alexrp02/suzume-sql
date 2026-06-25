//! The in-app connection editor: a draft connection plus the form's transient
//! editing state, used by the create/edit overlay.
//!
//! A draft holds raw, possibly-incomplete input. It only becomes a
//! [`NamedConnection`] through [`ConnectionDraft::validate`], so an invalid or
//! half-entered connection can never reach the config.

use crate::app::editor::TextInput;
use crate::config::{ConnectionConfig, NamedConnection};
use crate::worker::TestHandle;

/// The engine a draft targets. Mirrors [`ConnectionConfig`]'s variants without
/// their parameters, so the engine can be chosen before any value is entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftEngine {
    Sqlite,
    Postgres,
    Mysql,
}

impl DraftEngine {
    pub fn label(self) -> &'static str {
        match self {
            DraftEngine::Sqlite => "sqlite",
            DraftEngine::Postgres => "postgres",
            DraftEngine::Mysql => "mysql",
        }
    }

    /// Label for the value field, which is a filesystem path for SQLite and a
    /// connection URL for the networked engines.
    pub fn value_label(self) -> &'static str {
        match self {
            DraftEngine::Sqlite => "Path",
            DraftEngine::Postgres | DraftEngine::Mysql => "URL",
        }
    }

    pub fn next(self) -> DraftEngine {
        match self {
            DraftEngine::Sqlite => DraftEngine::Postgres,
            DraftEngine::Postgres => DraftEngine::Mysql,
            DraftEngine::Mysql => DraftEngine::Sqlite,
        }
    }

    pub fn prev(self) -> DraftEngine {
        match self {
            DraftEngine::Sqlite => DraftEngine::Mysql,
            DraftEngine::Postgres => DraftEngine::Sqlite,
            DraftEngine::Mysql => DraftEngine::Postgres,
        }
    }
}

/// Which field of the form has focus, and—for the text fields—whether it is
/// being edited (insert mode) or merely navigated (normal mode).
///
/// The engine selector carries no `editing` flag, so "inserting text into the
/// engine" is structurally impossible: the form is modal, but only the fields
/// that have a buffer can ever be in insert mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormFocus {
    Name { editing: bool },
    Engine,
    Value { editing: bool },
}

impl FormFocus {
    /// Move to the next field, landing in normal (non-editing) mode.
    pub fn next(self) -> FormFocus {
        match self {
            FormFocus::Name { .. } => FormFocus::Engine,
            FormFocus::Engine => FormFocus::Value { editing: false },
            FormFocus::Value { .. } => FormFocus::Name { editing: false },
        }
    }

    /// Move to the previous field, landing in normal (non-editing) mode.
    pub fn prev(self) -> FormFocus {
        match self {
            FormFocus::Name { .. } => FormFocus::Value { editing: false },
            FormFocus::Engine => FormFocus::Name { editing: false },
            FormFocus::Value { .. } => FormFocus::Engine,
        }
    }

    pub fn is_editing(self) -> bool {
        matches!(
            self,
            FormFocus::Name { editing: true } | FormFocus::Value { editing: true }
        )
    }
}

/// Why a [`ConnectionDraft`] failed to validate. Display text lives in
/// [`DraftError::message`], keeping the validation logic free of UI strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftError {
    MissingName,
    MissingTarget,
}

impl DraftError {
    pub fn message(self) -> &'static str {
        match self {
            DraftError::MissingName => "Name is required",
            DraftError::MissingTarget => "Connection target is required",
        }
    }
}

/// The state of the form's connection test. `Testing` owns the in-flight
/// [`TestHandle`], so "a test is running" and "we hold its handle" are the same
/// fact — neither can exist without the other.
#[derive(Debug)]
pub enum TestStatus {
    Idle,
    Testing(TestHandle),
    Ok,
    Failed(String),
}

/// A connection being created or edited.
pub struct ConnectionDraft {
    pub name: TextInput,
    pub engine: DraftEngine,
    pub value: TextInput,
    pub focus: FormFocus,
    /// `Some(index)` when editing an existing connection; `None` when creating.
    pub editing: Option<usize>,
    pub test: TestStatus,
}

impl ConnectionDraft {
    /// A blank draft for creating a new connection.
    pub fn new() -> ConnectionDraft {
        ConnectionDraft {
            name: TextInput::default(),
            engine: DraftEngine::Sqlite,
            value: TextInput::default(),
            focus: FormFocus::Name { editing: false },
            editing: None,
            test: TestStatus::Idle,
        }
    }

    /// A draft pre-filled from an existing connection, for editing.
    pub fn from_existing(index: usize, conn: &NamedConnection) -> ConnectionDraft {
        let (engine, value) = match &conn.connection {
            ConnectionConfig::Sqlite { path } => (DraftEngine::Sqlite, path.clone()),
            ConnectionConfig::Postgres { url } => (DraftEngine::Postgres, url.clone()),
            ConnectionConfig::Mysql { url } => (DraftEngine::Mysql, url.clone()),
        };
        ConnectionDraft {
            name: TextInput::with_text(&conn.name),
            engine,
            value: TextInput::with_text(&value),
            focus: FormFocus::Name { editing: false },
            editing: Some(index),
            test: TestStatus::Idle,
        }
    }

    /// The text input being edited, but only while a text field is in insert
    /// mode. In normal mode (or on the engine selector) there is nothing to type
    /// into, so this returns `None` and keystrokes can't mutate a buffer.
    pub fn editing_input(&mut self) -> Option<&mut TextInput> {
        match self.focus {
            FormFocus::Name { editing: true } => Some(&mut self.name),
            FormFocus::Value { editing: true } => Some(&mut self.value),
            _ => None,
        }
    }

    /// Enter insert mode on the focused text field. A no-op on the engine
    /// selector, which has no buffer.
    pub fn begin_edit(&mut self) {
        self.focus = match self.focus {
            FormFocus::Name { .. } => FormFocus::Name { editing: true },
            FormFocus::Value { .. } => FormFocus::Value { editing: true },
            FormFocus::Engine => FormFocus::Engine,
        };
    }

    /// Leave insert mode, staying on the same field.
    pub fn end_edit(&mut self) {
        self.focus = match self.focus {
            FormFocus::Name { .. } => FormFocus::Name { editing: false },
            FormFocus::Value { .. } => FormFocus::Value { editing: false },
            FormFocus::Engine => FormFocus::Engine,
        };
    }

    pub fn focus_next(&mut self) {
        self.focus = self.focus.next();
    }

    pub fn focus_prev(&mut self) {
        self.focus = self.focus.prev();
    }

    /// Cycle the engine selector, only when it is the focused field.
    pub fn cycle_engine(&mut self, forward: bool) {
        if self.focus == FormFocus::Engine {
            self.engine = if forward {
                self.engine.next()
            } else {
                self.engine.prev()
            };
        }
    }

    /// Validate the draft into a [`NamedConnection`], or return the first
    /// problem found.
    pub fn validate(&self) -> Result<NamedConnection, DraftError> {
        let name = self.name.text();
        let name = name.trim();
        if name.is_empty() {
            return Err(DraftError::MissingName);
        }
        let value = self.value.text();
        let value = value.trim();
        if value.is_empty() {
            return Err(DraftError::MissingTarget);
        }
        let connection = match self.engine {
            DraftEngine::Sqlite => ConnectionConfig::Sqlite {
                path: value.to_string(),
            },
            DraftEngine::Postgres => ConnectionConfig::Postgres {
                url: value.to_string(),
            },
            DraftEngine::Mysql => ConnectionConfig::Mysql {
                url: value.to_string(),
            },
        };
        Ok(NamedConnection {
            name: name.to_string(),
            connection,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_focus_only_lets_text_fields_enter_insert() {
        let mut draft = ConnectionDraft::new();
        // A fresh draft starts on Name, in normal mode.
        assert_eq!(draft.focus, FormFocus::Name { editing: false });
        assert!(draft.editing_input().is_none());

        // Entering insert on a text field exposes its buffer.
        draft.begin_edit();
        assert!(draft.focus.is_editing());
        assert!(draft.editing_input().is_some());

        // Enter from insert mode exits and advances to the next field.
        draft.focus_next();
        assert_eq!(draft.focus, FormFocus::Engine);
        assert!(!draft.focus.is_editing());

        // The engine selector has no buffer: begin_edit is a no-op, and it can't
        // be edited.
        draft.begin_edit();
        assert_eq!(draft.focus, FormFocus::Engine);
        assert!(draft.editing_input().is_none());
    }

    #[test]
    fn cycle_engine_only_acts_on_the_engine_field() {
        let mut draft = ConnectionDraft::new();
        // On the Name field, cycling does nothing.
        draft.cycle_engine(true);
        assert_eq!(draft.engine, DraftEngine::Sqlite);
        // On the Engine field, it advances through the variants.
        draft.focus = FormFocus::Engine;
        draft.cycle_engine(true);
        assert_eq!(draft.engine, DraftEngine::Postgres);
        draft.cycle_engine(false);
        assert_eq!(draft.engine, DraftEngine::Sqlite);
    }

    #[test]
    fn validate_requires_name_and_value() {
        let mut draft = ConnectionDraft::new();
        assert_eq!(draft.validate().unwrap_err(), DraftError::MissingName);
        draft.name = TextInput::with_text("local");
        assert_eq!(draft.validate().unwrap_err(), DraftError::MissingTarget);
        draft.value = TextInput::with_text("./demo.db");
        let conn = draft.validate().expect("valid");
        assert_eq!(conn.name, "local");
        match conn.connection {
            ConnectionConfig::Sqlite { path } => assert_eq!(path, "./demo.db"),
            other => panic!("expected sqlite, got {other:?}"),
        }
    }

    #[test]
    fn from_existing_round_trips_engine_and_value() {
        let conn = NamedConnection {
            name: "prod".to_string(),
            connection: ConnectionConfig::Postgres {
                url: "postgresql://u:p@host/app".to_string(),
            },
        };
        let draft = ConnectionDraft::from_existing(2, &conn);
        assert_eq!(draft.editing, Some(2));
        assert_eq!(draft.engine, DraftEngine::Postgres);
        assert_eq!(draft.value.text(), "postgresql://u:p@host/app");
        let rebuilt = draft.validate().expect("valid");
        match rebuilt.connection {
            ConnectionConfig::Postgres { url } => assert_eq!(url, "postgresql://u:p@host/app"),
            other => panic!("expected postgres, got {other:?}"),
        }
    }
}
