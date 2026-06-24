//! The fuzzy table finder: a Telescope/Ctrl-P style overlay for jumping to a
//! table by typing a fuzzy query, for databases with many tables.

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use crate::app::editor::TextInput;

/// Rank `names` against a fuzzy `query`, returning the matching indices into
/// `names`, best match first. An empty query keeps the original order.
pub fn fuzzy_rank(names: &[String], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..names.len()).collect();
    }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(usize, i64)> = names
        .iter()
        .enumerate()
        .filter_map(|(i, name)| matcher.fuzzy_match(name, query).map(|score| (i, score)))
        .collect();
    // Higher score first; ties keep the original (stable) order.
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

/// Transient state for the fuzzy finder overlay.
pub struct FinderState {
    pub input: TextInput,
    /// All candidate names, captured when the finder opened. Indices into this
    /// vector match the catalog sidebar's ordering.
    names: Vec<String>,
    /// Indices into `names`, best match first.
    matches: Vec<usize>,
    /// Selected position within `matches`.
    selected: usize,
}

impl FinderState {
    pub fn new(names: Vec<String>) -> FinderState {
        let mut state = FinderState {
            input: TextInput::default(),
            names,
            matches: Vec::new(),
            selected: 0,
        };
        state.recompute();
        state
    }

    /// Re-rank against the current query. Called after every edit; resets the
    /// selection to the top match.
    pub fn recompute(&mut self) {
        self.matches = fuzzy_rank(&self.names, &self.input.text());
        self.selected = 0;
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, last as isize) as usize;
    }

    /// The catalog index of the currently selected match, if any.
    pub fn selected_index(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    pub fn selected_position(&self) -> usize {
        self.selected
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    pub fn total_count(&self) -> usize {
        self.names.len()
    }

    /// The matched names in ranked order, for rendering.
    pub fn matched_names(&self) -> Vec<&str> {
        self.matches
            .iter()
            .filter_map(|&i| self.names.get(i).map(String::as_str))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        ["users", "products", "user_roles", "orders"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn empty_query_keeps_original_order() {
        assert_eq!(fuzzy_rank(&names(), ""), vec![0, 1, 2, 3]);
        assert_eq!(fuzzy_rank(&names(), "   "), vec![0, 1, 2, 3]);
    }

    #[test]
    fn subsequence_matches_only() {
        // "usr" is a subsequence of users (idx 0) and user_roles (idx 2),
        // but not of products or orders.
        let ranked = fuzzy_rank(&names(), "usr");
        assert!(ranked.contains(&0));
        assert!(ranked.contains(&2));
        assert!(!ranked.contains(&1));
        assert!(!ranked.contains(&3));
    }

    #[test]
    fn exact_prefix_ranks_first() {
        let ranked = fuzzy_rank(&names(), "users");
        assert_eq!(ranked.first(), Some(&0));
    }

    #[test]
    fn no_match_yields_empty() {
        assert!(fuzzy_rank(&names(), "zzz").is_empty());
    }

    #[test]
    fn selection_clamps_within_matches() {
        let mut finder = FinderState::new(names());
        finder.move_selection(100);
        assert_eq!(finder.selected_position(), finder.match_count() - 1);
        finder.move_selection(-100);
        assert_eq!(finder.selected_position(), 0);
    }
}
