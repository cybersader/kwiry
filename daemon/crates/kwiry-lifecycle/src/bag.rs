//! A tiny fixed-capacity sorted multiset.
//!
//! Model states must be small, hashable and canonical: two states that hold
//! the same messages in a different arrival order must compare equal. `Bag`
//! keeps its items sorted, so equality and hashing ignore insertion order.
//! Overflowing the capacity is a [`Bound`] error, never silent truncation: if
//! a bound is ever hit, the checker reports it as a failure, so every result
//! is known not to depend on a dropped message.

use std::fmt;

/// A model bound was exceeded. The checker treats this as a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound(pub &'static str);

impl fmt::Display for Bound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "model bound exceeded: {}", self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bag<T: Copy + Ord, const N: usize> {
    items: [Option<T>; N],
}

impl<T: Copy + Ord, const N: usize> Default for Bag<T, N> {
    fn default() -> Self {
        Self { items: [None; N] }
    }
}

impl<T: Copy + Ord + fmt::Debug, const N: usize> fmt::Debug for Bag<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Copy + Ord, const N: usize> Bag<T, N> {
    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.items.iter().map_while(|x| *x)
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.items[0].is_none()
    }

    pub fn get(&self, idx: usize) -> Option<T> {
        self.items.get(idx).copied().flatten()
    }

    pub fn contains(&self, x: &T) -> bool {
        self.iter().any(|y| y == *x)
    }

    fn normalize(&mut self) {
        // `None` sorts first for `Option`; we want the items first.
        self.items.sort_by(|a, b| match (a, b) {
            (Some(a), Some(b)) => a.cmp(b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
    }

    /// Multiset insert (duplicates allowed).
    pub fn push(&mut self, x: T, what: &'static str) -> Result<(), Bound> {
        let slot = self
            .items
            .iter_mut()
            .find(|s| s.is_none())
            .ok_or(Bound(what))?;
        *slot = Some(x);
        self.normalize();
        Ok(())
    }

    /// Set insert: no-op if already present.
    pub fn insert(&mut self, x: T, what: &'static str) -> Result<(), Bound> {
        if self.contains(&x) {
            return Ok(());
        }
        self.push(x, what)
    }

    pub fn remove_at(&mut self, idx: usize) -> Option<T> {
        let out = self.items.get_mut(idx)?.take();
        self.normalize();
        out
    }

    pub fn remove(&mut self, x: &T) -> bool {
        let found = self.iter().position(|y| y == *x);
        match found {
            Some(i) => {
                self.remove_at(i);
                true
            }
            None => false,
        }
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        for slot in self.items.iter_mut() {
            if let Some(x) = slot
                && !keep(x)
            {
                *slot = None;
            }
        }
        self.normalize();
    }

    pub fn map_in_place(&mut self, mut f: impl FnMut(&mut T)) {
        for x in self.items.iter_mut().flatten() {
            f(x);
        }
        self.normalize();
    }

    pub fn clear(&mut self) {
        self.items = [None; N];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_regardless_of_order() {
        let mut a: Bag<u8, 3> = Bag::default();
        let mut b: Bag<u8, 3> = Bag::default();
        a.push(2, "t").unwrap();
        a.push(1, "t").unwrap();
        b.push(1, "t").unwrap();
        b.push(2, "t").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.get(0), Some(1));
        assert!(a.push(3, "t").is_ok());
        assert_eq!(a.push(4, "t"), Err(Bound("t")));
        assert!(a.remove(&2));
        assert_eq!(a.iter().collect::<Vec<_>>(), vec![1, 3]);
    }
}
