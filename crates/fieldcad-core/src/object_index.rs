//! O(1) lookup by [`ObjectId`] over a collection of object-keyed items.
//!
//! Every analytic force plugin (electrostatics, gravity, ...) and the EM
//! particle coupling need the same thing: given an `ObjectId`, find the one
//! matching source/particle among many, often while excluding that id from
//! an aggregate over the rest. `ObjectId` is already `Hash + Eq`, so this is
//! answered once here instead of once per plugin with a hand-rolled
//! `HashMap`.

use std::collections::HashMap;

use crate::ObjectId;

/// An item that can be located by the [`ObjectId`] of the object it belongs to.
pub trait IdentifiedByObject {
    fn object_id(&self) -> ObjectId;
}

impl<T> IdentifiedByObject for crate::CoupledSource<T> {
    fn object_id(&self) -> ObjectId {
        self.object
    }
}

/// Maps each item's [`ObjectId`] to its position in `items`.
///
/// For borrowed slices that are rebuilt on every call (e.g. a per-tick
/// particle list the caller doesn't own), use this directly rather than
/// through [`ObjectIndex`].
pub fn index_by_object<T: IdentifiedByObject>(items: &[T]) -> HashMap<ObjectId, usize> {
    items
        .iter()
        .enumerate()
        .map(|(position, item)| (item.object_id(), position))
        .collect()
}

/// An owned `Vec<T>` paired with an [`ObjectId`]-keyed index over it.
///
/// Built once from a `Vec` (typically inside a solver's `on_world_changed`)
/// and reused across every `forces()`/`sample()` call until the backing
/// `Vec` is replaced, turning per-body linear scans into O(1) lookups.
pub struct ObjectIndex<T> {
    items: Vec<T>,
    index: HashMap<ObjectId, usize>,
}

impl<T: IdentifiedByObject> ObjectIndex<T> {
    pub fn new(items: Vec<T>) -> Self {
        let index = index_by_object(&items);
        Self { items, index }
    }

    pub fn get(&self, id: ObjectId) -> Option<&T> {
        self.index.get(&id).map(|&position| &self.items[position])
    }

    /// The position of the item identified by `id` in [`Self::as_slice`],
    /// or `None` if no such item is held.
    ///
    /// Inverse of slicing: `as_slice()[..i].iter().chain(as_slice()[i + 1..].iter())`
    /// yields exactly what [`Self::iter_excluding`] yields, in the same
    /// order — the shape a hot path that already holds a precomputed,
    /// positionally-aligned copy of the items needs to exclude one item
    /// with a single lookup and no per-item conversion.
    pub fn index_of(&self, id: ObjectId) -> Option<usize> {
        self.index.get(&id).copied()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }

    /// Every item except the one identified by `id`, in the collection's
    /// original order. If `id` isn't present, yields every item.
    pub fn iter_excluding(&self, id: ObjectId) -> impl Iterator<Item = &T> {
        let excluded = self.index.get(&id).copied();
        self.items
            .iter()
            .enumerate()
            .filter_map(move |(position, item)| (Some(position) != excluded).then_some(item))
    }

    pub fn as_slice(&self) -> &[T] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Item {
        object: ObjectId,
        value: u32,
    }

    impl IdentifiedByObject for Item {
        fn object_id(&self) -> ObjectId {
            self.object
        }
    }

    fn item(id: u64, value: u32) -> Item {
        Item {
            object: ObjectId::new(id),
            value,
        }
    }

    #[test]
    fn get_finds_the_matching_item() {
        let index = ObjectIndex::new(vec![item(1, 10), item(2, 20), item(3, 30)]);
        assert_eq!(index.get(ObjectId::new(2)), Some(&item(2, 20)));
    }

    #[test]
    fn get_returns_none_for_an_absent_id() {
        let index = ObjectIndex::new(vec![item(1, 10)]);
        assert_eq!(index.get(ObjectId::new(99)), None);
    }

    #[test]
    fn iter_excluding_skips_only_the_matching_item_and_keeps_order() {
        let index = ObjectIndex::new(vec![item(1, 10), item(2, 20), item(3, 30)]);
        let remaining: Vec<u32> = index
            .iter_excluding(ObjectId::new(2))
            .map(|item| item.value)
            .collect();
        assert_eq!(remaining, vec![10, 30]);
    }

    #[test]
    fn iter_excluding_yields_everything_when_id_is_absent() {
        let index = ObjectIndex::new(vec![item(1, 10), item(2, 20)]);
        let remaining: Vec<u32> = index
            .iter_excluding(ObjectId::new(99))
            .map(|item| item.value)
            .collect();
        assert_eq!(remaining, vec![10, 20]);
    }

    #[test]
    fn index_of_returns_the_position_in_as_slice() {
        let index = ObjectIndex::new(vec![item(1, 10), item(2, 20), item(3, 30)]);
        assert_eq!(index.index_of(ObjectId::new(2)), Some(1));
        assert_eq!(index.index_of(ObjectId::new(1)), Some(0));
        assert_eq!(index.index_of(ObjectId::new(99)), None);
    }

    /// The contract `index_of` exists to serve: skipping one position over
    /// `as_slice` must be indistinguishable from `iter_excluding`, item
    /// for item and in order.
    #[test]
    fn excluding_by_index_matches_iter_excluding() {
        let index = ObjectIndex::new(vec![item(1, 10), item(2, 20), item(3, 30)]);
        for id in [
            ObjectId::new(1),
            ObjectId::new(2),
            ObjectId::new(3),
            ObjectId::new(99),
        ] {
            let by_iterator: Vec<u32> = index.iter_excluding(id).map(|item| item.value).collect();
            let by_index: Vec<u32> = match index.index_of(id) {
                Some(excluded) => index
                    .as_slice()
                    .iter()
                    .enumerate()
                    .filter_map(|(position, item)| (position != excluded).then_some(item.value))
                    .collect(),
                None => index.as_slice().iter().map(|item| item.value).collect(),
            };
            assert_eq!(by_iterator, by_index, "diverged excluding {id:?}");
        }
    }

    #[test]
    fn index_by_object_matches_object_index_positions() {
        let items = vec![item(5, 50), item(6, 60)];
        let map = index_by_object(&items);
        assert_eq!(map.get(&ObjectId::new(6)), Some(&1));
    }
}
