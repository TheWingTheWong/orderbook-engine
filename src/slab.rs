//! A minimal slab allocator: a `Vec` of slots with an intrusive free list.
//!
//! Resting orders live in a single contiguous slab and refer to each other by
//! `u32` index. Removing an order pushes its slot onto the free list, and the
//! next insertion reuses it, so a book in steady state performs no heap
//! allocation and keeps its working set dense.

/// Index of a slot inside a [`Slab`].
pub type SlotId = u32;

/// Sentinel meaning "no slot" (used for list terminators).
pub const NIL: SlotId = SlotId::MAX;

#[derive(Debug, Clone)]
enum Slot<T> {
    Occupied(T),
    Vacant { next_free: SlotId },
}

/// Fixed-type object pool with O(1) insert, remove and lookup.
#[derive(Debug, Clone)]
pub struct Slab<T> {
    slots: Vec<Slot<T>>,
    free_head: SlotId,
    len: usize,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Slab<T> {
    /// Empty slab.
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// Empty slab with room for `cap` values before reallocating.
    pub fn with_capacity(cap: usize) -> Self {
        Self { slots: Vec::with_capacity(cap), free_head: NIL, len: 0 }
    }

    /// Number of occupied slots.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no slot is occupied.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Total slots ever allocated (occupied + free).
    #[inline]
    pub fn capacity_used(&self) -> usize {
        self.slots.len()
    }

    /// Store `value`, returning its slot id. Reuses a freed slot if any.
    #[inline]
    pub fn insert(&mut self, value: T) -> SlotId {
        self.len += 1;
        if self.free_head != NIL {
            let id = self.free_head;
            let slot = &mut self.slots[id as usize];
            match *slot {
                Slot::Vacant { next_free } => self.free_head = next_free,
                Slot::Occupied(_) => unreachable!("free list points at occupied slot"),
            }
            *slot = Slot::Occupied(value);
            id
        } else {
            let id = self.slots.len();
            assert!(id < NIL as usize, "slab exhausted u32 index space");
            self.slots.push(Slot::Occupied(value));
            id as SlotId
        }
    }

    /// Remove and return the value at `id`.
    ///
    /// # Panics
    /// If `id` is not occupied.
    #[inline]
    pub fn remove(&mut self, id: SlotId) -> T {
        let slot = &mut self.slots[id as usize];
        match std::mem::replace(slot, Slot::Vacant { next_free: self.free_head }) {
            Slot::Occupied(v) => {
                self.free_head = id;
                self.len -= 1;
                v
            }
            vacant @ Slot::Vacant { .. } => {
                *slot = vacant;
                panic!("slab: remove of vacant slot {id}");
            }
        }
    }

    /// Shared reference to the value at `id`, if occupied.
    #[inline]
    pub fn get(&self, id: SlotId) -> Option<&T> {
        match self.slots.get(id as usize) {
            Some(Slot::Occupied(v)) => Some(v),
            _ => None,
        }
    }

    /// Mutable reference to the value at `id`, if occupied.
    #[inline]
    pub fn get_mut(&mut self, id: SlotId) -> Option<&mut T> {
        match self.slots.get_mut(id as usize) {
            Some(Slot::Occupied(v)) => Some(v),
            _ => None,
        }
    }

    /// Iterate over `(id, &value)` for every occupied slot.
    pub fn iter(&self) -> impl Iterator<Item = (SlotId, &T)> + '_ {
        self.slots.iter().enumerate().filter_map(|(i, s)| match s {
            Slot::Occupied(v) => Some((i as SlotId, v)),
            Slot::Vacant { .. } => None,
        })
    }
}

impl<T> std::ops::Index<SlotId> for Slab<T> {
    type Output = T;

    #[inline]
    fn index(&self, id: SlotId) -> &T {
        match &self.slots[id as usize] {
            Slot::Occupied(v) => v,
            Slot::Vacant { .. } => panic!("slab: access to vacant slot {id}"),
        }
    }
}

impl<T> std::ops::IndexMut<SlotId> for Slab<T> {
    #[inline]
    fn index_mut(&mut self, id: SlotId) -> &mut T {
        match &mut self.slots[id as usize] {
            Slot::Occupied(v) => v,
            Slot::Vacant { .. } => panic!("slab: access to vacant slot {id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove() {
        let mut s = Slab::new();
        let a = s.insert("a");
        let b = s.insert("b");
        assert_eq!(s.len(), 2);
        assert_eq!(s[a], "a");
        assert_eq!(s.remove(a), "a");
        assert_eq!(s.get(a), None);
        assert_eq!(s[b], "b");
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn freed_slots_are_reused_lifo() {
        let mut s = Slab::new();
        let ids: Vec<_> = (0..4).map(|i| s.insert(i)).collect();
        s.remove(ids[1]);
        s.remove(ids[3]);
        assert_eq!(s.insert(10), ids[3]);
        assert_eq!(s.insert(11), ids[1]);
        assert_eq!(s.insert(12), 4);
        assert_eq!(s.capacity_used(), 5);
        let mut live: Vec<_> = s.iter().map(|(_, v)| *v).collect();
        live.sort();
        assert_eq!(live, vec![0, 2, 10, 11, 12]);
    }

    #[test]
    #[should_panic(expected = "vacant")]
    fn double_remove_panics() {
        let mut s = Slab::new();
        let a = s.insert(1);
        s.remove(a);
        s.remove(a);
    }

    #[test]
    fn get_mut_out_of_range_is_none() {
        let mut s: Slab<u8> = Slab::with_capacity(8);
        assert!(s.get_mut(3).is_none());
        assert!(s.is_empty());
    }
}
