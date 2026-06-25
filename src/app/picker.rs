//! The connection picker: a fuzzy-filterable list of configured connections,
//! reachable at startup and from the browser.
//!
//! The fuzzy matching reuses [`FinderState`] over the connection names; the
//! ranked indices map back into [`Config::connections`].

use crate::app::finder::FinderState;
use crate::config::Config;

/// A confirmation the picker is waiting on before a destructive action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerPrompt {
    /// Awaiting a second keypress to delete the connection at this index.
    ConfirmDelete(usize),
    /// Awaiting confirmation to connect to this index, discarding pending edits.
    ConfirmSwitch(usize),
}

/// Transient state for the connection picker overlay.
pub struct PickerState {
    pub finder: FinderState,
    pub prompt: Option<PickerPrompt>,
}

impl PickerState {
    pub fn new(config: &Config) -> PickerState {
        let names = config.connections.iter().map(|c| c.name.clone()).collect();
        PickerState {
            finder: FinderState::new(names),
            prompt: None,
        }
    }

    /// The config index of the highlighted connection, if any.
    pub fn selected_connection(&self) -> Option<usize> {
        self.finder.selected_index()
    }
}
