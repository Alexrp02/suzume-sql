//! The cell/row inspector overlay: a scrollable modal showing the full value of
//! the selected cell, or every column/value pair of the selected row.
//!
//! Inspection never mutates the grid and is available even on read-only
//! relations, so the inspector owns a snapshot of the values taken when it
//! opens. Rendering (wrapping, styling) lives in [`crate::ui`]; this module is
//! pure state.

use crate::model::value::Value;

/// What the inspector is showing. The variant fixes the shape of the view, so a
/// row inspector can never be missing its fields and a cell inspector can never
/// carry more than one value.
pub enum InspectTarget {
    Cell { column: String, value: Value },
    Row { fields: Vec<(String, Value)> },
}

/// Transient state for the inspector overlay: the snapshot under inspection plus
/// the vertical scroll offset, in wrapped display lines.
pub struct InspectState {
    pub target: InspectTarget,
    pub scroll: u16,
}

impl InspectState {
    pub fn cell(column: String, value: Value) -> InspectState {
        InspectState {
            target: InspectTarget::Cell { column, value },
            scroll: 0,
        }
    }

    pub fn row(fields: Vec<(String, Value)>) -> InspectState {
        InspectState {
            target: InspectTarget::Row { fields },
            scroll: 0,
        }
    }

    /// Scroll by `delta` display lines, clamped at the top. The bottom is clamped
    /// at render time, where the wrapped content height is known.
    pub fn scroll_by(&mut self, delta: isize) {
        let next = (self.scroll as isize + delta).max(0);
        self.scroll = u16::try_from(next).unwrap_or(u16::MAX);
    }
}
