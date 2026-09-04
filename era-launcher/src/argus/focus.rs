//! FocusManager — single canonical focus registry for ARGUS.
//!
//! Guarantees:
//![FocusManager] next() advances exactly one target.
//! TAB: move exactly one focus target forward
//! SHIFT+TAB: move exactly one focus target backward
//! ENTER: activate the currently focused target
//! CLICK: select that target and activate it where appropriate
//! No double advancement. No duplicate registration.

use std::collections::HashMap;

/// A focusable target in the UI.
/// Each target is identified by a unique string key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FocusTarget {
    pub id: String,
    pub label: String,
}

impl FocusTarget {
    pub fn new(id: &str, label: &str) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
        }
    }
}

/// Single canonical focus system.
///
/// Maintains exactly one focus index across all registered targets.
/// All keyboard navigation and mouse navigation use this same registry.
#[derive(Debug, Clone)]
pub struct FocusManager {
    targets: Vec<FocusTarget>,
    current_index: usize,
    id_to_index: HashMap<String, usize>,
}

impl FocusManager {
    pub fn new() -> Self {
        Self {
            targets: Vec::new(),
            current_index: 0,
            id_to_index: HashMap::new(),
        }
    }

    /// Register a focusable target. Panics if a duplicate id is added.
    pub fn register(&mut self, id: &str, label: &str) -> usize {
        assert!(
            !self.id_to_index.contains_key(id),
            "Duplicate focus target registration: {}",
            id
        );
        let index = self.targets.len();
        self.targets.push(FocusTarget::new(id, label));
        self.id_to_index.insert(id.to_string(), index);
        index
    }

    /// Register multiple targets at once.
    pub fn register_all(&mut self, targets: &[(&str, &str)]) {
        for (id, label) in targets {
            self.register(id, label);
        }
    }

    /// Move focus exactly one step forward. Wraps around.
    pub fn next(&mut self) {
        if self.targets.is_empty() {
            return;
        }
        self.current_index = (self.current_index + 1) % self.targets.len();
    }

    /// Move focus exactly one step backward. Wraps around.
    pub fn previous(&mut self) {
        if self.targets.is_empty() {
            return;
        }
        if self.current_index == 0 {
            self.current_index = self.targets.len() - 1;
        } else {
            self.current_index -= 1;
        }
    }

    /// Set focus to a specific index.
    pub fn set(&mut self, index: usize) {
        if !self.targets.is_empty() {
            self.current_index = index.min(self.targets.len() - 1);
        }
    }

    /// Set focus to the first target (index 0).
    pub fn first(&mut self) {
        self.current_index = 0;
    }

    /// Set focus by target id.
    pub fn set_by_id(&mut self, id: &str) -> bool {
        if let Some(&idx) = self.id_to_index.get(id) {
            self.current_index = idx;
            true
        } else {
            false
        }
    }

    /// Get the currently focused target.
    pub fn current(&self) -> Option<&FocusTarget> {
        self.targets.get(self.current_index)
    }

    /// Get the current index.
    pub fn current_index(&self) -> usize {
        self.current_index
    }

    /// Get the total number of targets.
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Get all targets.
    pub fn targets(&self) -> &[FocusTarget] {
        &self.targets
    }

    /// Find a target by id and return its index.
    pub fn find(&self, id: &str) -> Option<usize> {
        self.id_to_index.get(id).copied()
    }

    /// Clear all targets and reset.
    pub fn clear(&mut self) {
        self.targets.clear();
        self.id_to_index.clear();
        self.current_index = 0;
    }

    /// Rebuild the focus registry from a list of targets.
    /// This replaces all existing targets.
    pub fn rebuild(&mut self, targets: &[(&str, &str)]) {
        self.clear();
        self.register_all(targets);
    }
}

impl Default for FocusManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fm() -> FocusManager {
        let mut fm = FocusManager::new();
        fm.register_all(&[
            ("nav_home", "HOME"),
            ("nav_discover", "DISCOVER"),
            ("nav_instances", "INSTANCES"),
            ("nav_mods", "MODS"),
            ("nav_exit", "EXIT"),
        ]);
        fm
    }

    #[test]
    fn test_tab_advances_one_step() {
        let mut fm = make_fm();
        assert_eq!(fm.current_index(), 0);
        assert_eq!(fm.current().unwrap().id, "nav_home");
        fm.next();
        assert_eq!(fm.current_index(), 1);
        assert_eq!(fm.current().unwrap().id, "nav_discover");
    }

    #[test]
    fn test_shift_tab_advances_one_backward() {
        let mut fm = make_fm();
        fm.set(2);
        assert_eq!(fm.current().unwrap().id, "nav_instances");
        fm.previous();
        assert_eq!(fm.current_index(), 1);
        assert_eq!(fm.current().unwrap().id, "nav_discover");
    }

    #[test]
    fn test_tab_wraps_to_start() {
        let mut fm = make_fm();
        fm.set(4);
        assert_eq!(fm.current().unwrap().id, "nav_exit");
        fm.next();
        assert_eq!(fm.current_index(), 0);
        assert_eq!(fm.current().unwrap().id, "nav_home");
    }

    #[test]
    fn test_shift_tab_wraps_to_end() {
        let mut fm = make_fm();
        assert_eq!(fm.current_index(), 0);
        fm.previous();
        assert_eq!(fm.current_index(), 4);
        assert_eq!(fm.current().unwrap().id, "nav_exit");
    }

    #[test]
    fn test_set_by_id() {
        let mut fm = make_fm();
        assert!(fm.set_by_id("nav_mods"));
        assert_eq!(fm.current_index(), 3);
        assert_eq!(fm.current().unwrap().id, "nav_mods");
        assert!(fm.set_by_id("nav_home"));
        assert_eq!(fm.current_index(), 0);
        assert!(!fm.set_by_id("nonexistent"));
    }

    #[test]
    fn test_no_duplicate_registration() {
        let mut fm = FocusManager::new();
        fm.register("a", "A");
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fm.register("a", "A");
            }))
            .is_err()
        );
    }

    #[test]
    fn test_clear_resets() {
        let mut fm = make_fm();
        assert_eq!(fm.len(), 5);
        fm.clear();
        assert_eq!(fm.len(), 0);
        assert_eq!(fm.current_index(), 0);
        assert!(fm.current().is_none());
    }

    #[test]
    fn test_rebuild_replaces_all() {
        let mut fm = make_fm();
        assert_eq!(fm.len(), 5);
        fm.rebuild(&[("x", "X"), ("y", "Y")]);
        assert_eq!(fm.len(), 2);
        assert_eq!(fm.current().unwrap().id, "x");
        assert!(fm.set_by_id("y"));
        assert_eq!(fm.current().unwrap().id, "y");
    }

    #[test]
    fn test_empty_does_not_panic() {
        let mut fm = FocusManager::new();
        fm.next();
        fm.previous();
        assert!(fm.current().is_none());
    }

    #[test]
    fn test_find_by_id() {
        let fm = make_fm();
        assert_eq!(fm.find("nav_home"), Some(0));
        assert_eq!(fm.find("nav_discover"), Some(1));
        assert_eq!(fm.find("nav_instances"), Some(2));
        assert_eq!(fm.find("nav_mods"), Some(3));
        assert_eq!(fm.find("nav_exit"), Some(4));
        assert_eq!(fm.find("nonexistent"), None);
    }

    #[test]
    fn test_single_target_wraps() {
        let mut fm = FocusManager::new();
        fm.register("only", "Only");
        fm.next();
        assert_eq!(fm.current_index(), 0);
        fm.previous();
        assert_eq!(fm.current_index(), 0);
    }
}
