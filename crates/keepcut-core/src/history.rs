//! Snapshot-based undo/redo (§9). Push a clone of the state *before* each edit;
//! a drag is pushed once, on release.

pub struct History<T> {
    undo: Vec<T>,
    redo: Vec<T>,
    cap: usize,
}

impl<T: Clone> History<T> {
    pub fn new(cap: usize) -> Self {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            cap,
        }
    }

    /// Records `before` (the state prior to an edit) and clears the redo stack.
    pub fn push(&mut self, before: T) {
        self.undo.push(before);
        if self.undo.len() > self.cap {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Returns the state to restore; `current` goes onto the redo stack.
    pub fn undo(&mut self, current: T) -> Option<T> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        Some(prev)
    }

    pub fn redo(&mut self, current: T) -> Option<T> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_cap() {
        let mut h = History::new(2);
        let mut v = 0;
        for next in 1..=3 {
            h.push(v);
            v = next;
        }
        assert_eq!(v, 3);
        v = h.undo(v).unwrap(); // 2
        v = h.undo(v).unwrap(); // 1
        assert_eq!(v, 1);
        assert!(h.undo(v).is_none(), "cap of 2 dropped the oldest");
        v = h.redo(v).unwrap();
        assert_eq!(v, 2);
        h.push(v);
        assert!(!h.can_redo(), "a new edit clears redo");
    }
}
