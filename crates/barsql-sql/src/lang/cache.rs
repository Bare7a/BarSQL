use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

// Eviction scans for the oldest entry, which is cheap at these sizes.
#[derive(Debug)]
pub(crate) struct Lru<K, V> {
    map: HashMap<K, (V, u64)>,
    tick: u64,
    capacity: usize,
}

impl<K: Eq + Hash + Clone, V: Clone> Lru<K, V> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self { map: HashMap::new(), tick: 0, capacity }
    }

    pub(crate) fn get<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(key).map(|(value, used)| {
            *used = tick;
            value.clone()
        })
    }

    pub(crate) fn insert(&mut self, key: K, value: V) {
        self.tick += 1;
        self.map.insert(key, (value, self.tick));
        if self.map.len() > self.capacity
            && let Some(oldest) = self.map.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone())
        {
            self.map.remove(&oldest);
        }
    }
}
