//! A list model like Slint's `VecModel`, which also takes several rows at once: a page of
//! older messages goes in above as one change, not one per message (each of which would shift
//! the rows after it and make the list look again).

use std::any::Any;
use std::cell::RefCell;

use slint::{Model, ModelNotify, ModelTracker};

pub(crate) struct Rows<T> {
    rows: RefCell<Vec<T>>,
    notify: ModelNotify,
}

impl<T> Default for Rows<T> {
    fn default() -> Self {
        Self {
            rows: RefCell::default(),
            notify: ModelNotify::default(),
        }
    }
}

impl<T: Clone + 'static> Model for Rows<T> {
    type Data = T;

    fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<T> {
        self.rows.borrow().get(row).cloned()
    }

    fn set_row_data(&self, row: usize, data: T) {
        // Released before telling: whoever listens may read the rows right away.
        let set = match self.rows.borrow_mut().get_mut(row) {
            Some(slot) => {
                *slot = data;
                true
            }
            None => false,
        };
        if set {
            self.notify.row_changed(row);
        }
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl<T: Clone + 'static> Rows<T> {
    /// Puts `items` above the first row.
    pub(crate) fn prepend(&self, items: Vec<T>) {
        let count = items.len();
        if count == 0 {
            return;
        }
        self.rows.borrow_mut().splice(0..0, items);
        self.notify.row_added(0, count);
    }

    /// Puts `items` after the last row.
    pub(crate) fn extend(&self, items: impl IntoIterator<Item = T>) {
        let (start, count) = {
            let mut rows = self.rows.borrow_mut();
            let start = rows.len();
            rows.extend(items);
            (start, rows.len() - start)
        };
        if count > 0 {
            self.notify.row_added(start, count);
        }
    }

    /// Replaces every row.
    pub(crate) fn set_vec(&self, items: Vec<T>) {
        *self.rows.borrow_mut() = items;
        self.notify.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_go_in_where_asked() {
        let rows = Rows::default();
        rows.extend([3, 4]);
        rows.prepend(vec![1, 2]);
        rows.extend([5]);
        rows.set_row_data(0, 0);
        // Past the end: nothing to set.
        rows.set_row_data(9, 9);
        assert_eq!(rows.iter().collect::<Vec<_>>(), [0, 2, 3, 4, 5]);
        rows.set_vec(vec![7]);
        assert_eq!(rows.row_count(), 1);
    }
}
