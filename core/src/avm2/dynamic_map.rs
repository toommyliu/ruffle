use crate::avm2::object::{Object, TObject as _, WeakObject};
use crate::string::AvmString;
use fnv::FnvBuildHasher;
use gc_arena::Collect;
use hashbrown::HashTable;
use hashbrown::hash_table::Entry;
use std::cell::Cell;
use std::hash::{BuildHasher, Hash, Hasher};

#[derive(Debug, Collect, Copy, Clone)]
#[collect(no_drop)]
pub struct DynamicProperty<V> {
    pub value: V,
    pub enumerable: bool,
}

#[derive(Copy, Clone, Collect)]
#[collect(no_drop)]
pub enum DynamicKey<'gc> {
    String(AvmString<'gc>),
    // When the name parses as a non-negative integer, we use that integer as the key
    // See `ScriptObject::get_property_local` and `ScriptObject::set_property_local`.
    // This is observable when iterating over the object keys, as the key
    // can be `number`
    Uint(u32),
    Object(Object<'gc>),
    WeakObject(WeakObject<'gc>),
}

impl<'gc> DynamicKey<'gc> {
    fn object_ptr(&self) -> Option<*const crate::avm2::object::ObjectPtr> {
        match self {
            DynamicKey::Object(o) => Some(o.as_ptr()),
            DynamicKey::WeakObject(o) => Some(o.as_ptr()),
            _ => None,
        }
    }

    pub fn is_dead(&self) -> bool {
        matches!(self, DynamicKey::WeakObject(o) if o.is_dropped())
    }
}

impl PartialEq for DynamicKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (DynamicKey::String(a), DynamicKey::String(b)) => a == b,
            (DynamicKey::Uint(a), DynamicKey::Uint(b)) => a == b,
            // Comparing addresses is safe for weak keys: a GcWeak keeps its
            // allocation alive, so a dead key's address isn't reused.
            _ => match (self.object_ptr(), other.object_ptr()) {
                (Some(a), Some(b)) => std::ptr::eq(a, b),
                _ => false,
            },
        }
    }
}

impl Eq for DynamicKey<'_> {}

// Hashes exactly as `#[derive(Hash)]` did before `WeakObject` existed (the
// discriminant as an `isize`, then the field), since the hash decides bucket
// order and so the enumeration order that content can observe. A weak key
// hashes like the strong key for the same object.
impl Hash for DynamicKey<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            DynamicKey::String(s) => {
                0isize.hash(state);
                s.hash(state);
            }
            DynamicKey::Uint(u) => {
                1isize.hash(state);
                u.hash(state);
            }
            DynamicKey::Object(_) | DynamicKey::WeakObject(_) => {
                2isize.hash(state);
                self.object_ptr().expect("object key").hash(state);
            }
        }
    }
}

pub trait LiveKey {
    fn is_live(&self) -> bool {
        true
    }
}

impl LiveKey for DynamicKey<'_> {
    fn is_live(&self) -> bool {
        !self.is_dead()
    }
}

impl LiveKey for &str {}

/// A HashMap designed for dynamic properties on an object.
///
/// Uses `HashTable` directly to expose stable bucket indices, which are
/// needed for correct iteration when entries are added or removed mid-iteration.
#[derive(Debug, Clone, Collect)]
#[collect(no_drop)]
pub struct DynamicMap<K, V> {
    table: HashTable<(K, DynamicProperty<V>)>,
    #[collect(require_static)]
    hasher: FnvBuildHasher,
    // The last index that was given back to flash
    #[collect(require_static)]
    public_index: Cell<usize>,
    // The actual bucket index that represents where an item is in the table
    #[collect(require_static)]
    real_index: Cell<usize>,
}

impl<K: Eq + Hash + LiveKey, V> Default for DynamicMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + LiveKey, V> DynamicMap<K, V> {
    fn hash_key(&self, key: &K) -> u64 {
        self.hasher.hash_one(key)
    }

    pub fn new() -> Self {
        Self {
            table: HashTable::new(),
            hasher: FnvBuildHasher::default(),
            public_index: Cell::new(0),
            real_index: Cell::new(0),
        }
    }

    pub fn get(&self, key: &K) -> Option<&DynamicProperty<V>> {
        let hash = self.hash_key(key);

        self.table.find(hash, |(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        let hash = self.hash_key(key);

        self.table.find(hash, |(k, _)| k == key).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &DynamicProperty<V>)> {
        self.table.iter().map(|(k, v)| (k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.table.iter().map(|(k, _)| k)
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Gets the bucket index from the current public index.
    /// Returns `None` if the index is out of bounds.
    fn public_to_real_index(&self, index: usize) -> Option<usize> {
        let mut count = 0;
        let num_buckets = self.table.num_buckets();

        for i in 0..num_buckets {
            if let Some((k, v)) = self.table.get_bucket(i)
                && v.enumerable
                && k.is_live()
            {
                count += 1;

                if count >= index {
                    return Some(i);
                }
            }
        }

        None
    }

    pub fn insert(&mut self, key: K, new_value: V) {
        let hash = self.hash_key(&key);

        match self
            .table
            .entry(hash, |(k, _)| *k == key, |(k, _)| self.hasher.hash_one(k))
        {
            Entry::Occupied(mut occupied) => {
                // NOTE: When inserting a new value into an already-occupied entry,
                // the value of the `enumerable` field isn't reset to `true`
                occupied.get_mut().1.value = new_value;
            }
            Entry::Vacant(vacant) => {
                vacant.insert((
                    key,
                    DynamicProperty {
                        value: new_value,
                        enumerable: true,
                    },
                ));
            }
        }
    }

    pub fn remove(&mut self, key: &K) -> Option<DynamicProperty<V>> {
        let hash = self.hash_key(key);

        match self.table.find_entry(hash, |(k, _)| k == key) {
            Ok(occupied) => Some(occupied.remove().0.1),
            Err(_) => None,
        }
    }

    // NOTE: Per the docs on `HashTable::find_bucket_index`, bucket indices are
    // "only meaningful as long as the table is not resized and no entries are
    // added or removed". The current implementation uses tombstones and never
    // moves elements, so indices of other entries are preserved on removal in
    // practice, but this is not a guaranteed API contract and should not be
    // relied on long-term.
    // https://docs.rs/hashbrown/0.16.1/hashbrown/struct.HashTable.html#method.find_bucket_index
    pub fn next(&self, index: usize) -> Option<usize> {
        // Start iteration from the beginning
        if index == 0 {
            if let Some(real) = self.public_to_real_index(1) {
                self.real_index.set(real);
                self.public_index.set(1);
                return Some(1);
            } else {
                self.public_index.set(0);
                self.real_index.set(0);
                return None;
            }
        }
        // If the public index is zero than this is the first time this is being enumerated,
        // if index != self.public_index, then we are enumerating twice OR we mutated while enumerating.
        //
        // Regardless of the reason, we just need to sync the supplied index to the real index.
        if self.public_index.get() == 0 || index != self.public_index.get() {
            if let Some(real) = self.public_to_real_index(index) {
                // Pick up where we left off in the iteration
                // See https://github.com/adobe/avmplus/blob/858d034a3bd3a54d9b70909386435cf4aec81d21/core/avmplusHashtable.cpp#L395
                self.real_index.set(real);
                self.public_index.set(index);
            } else {
                self.public_index.set(0);
                self.real_index.set(0);
                return None;
            }
        }

        let real = self.real_index.get() + 1;
        let num_buckets = self.table.num_buckets();

        if !self.table.is_empty() && real < num_buckets {
            for i in real..num_buckets {
                if let Some((k, v)) = self.table.get_bucket(i)
                    && v.enumerable
                    && k.is_live()
                {
                    self.real_index.set(i);
                    self.public_index.set(self.public_index.get() + 1);
                    return Some(self.public_index.get());
                }
            }
        }

        None
    }

    pub fn pair_at(&self, index: usize) -> Option<&(K, DynamicProperty<V>)> {
        let real_index = if self.public_index.get() == 0 || self.public_index.get() != index {
            self.public_to_real_index(index)?
        } else {
            self.real_index.get()
        };

        self.table.get_bucket(real_index)
    }

    pub fn key_at(&self, index: usize) -> Option<&K> {
        self.pair_at(index).map(|(k, _)| k)
    }

    pub fn value_at(&self, index: usize) -> Option<&V> {
        self.pair_at(index).map(|(_, p)| &p.value)
    }

    pub fn prune_dead_keys(&mut self) {
        self.table.retain(|(k, _)| k.is_live());
    }

    pub fn set_enumerable(&mut self, key: &K, enumerable: bool) {
        let hash = self.hash_key(key);

        if let Some((_, prop)) = self.table.find_mut(hash, |(k, _)| k == key) {
            prop.enumerable = enumerable;
        }
    }
}

#[cfg(test)]
mod tests {

    use super::DynamicMap;

    #[test]
    fn test_dynamic_map() {
        let mut map: DynamicMap<&'static str, i32> = DynamicMap::new();
        map.insert("a", 0);
        map.insert("b", 0);
        map.insert("c", 0);
        map.insert("d", 0);
        let mut current = 0;
        while let Some(next) = map.next(current) {
            if current == 2 {
                map.insert("e", 0);
                map.insert("f", 0);
            }
            println!("{}", map.key_at(current).unwrap());
            current = next;
        }
        println!("next");
        current = 0;
        while let Some(next) = map.next(current) {
            println!("{}", map.key_at(current).unwrap());
            current = next;
        }
    }
}
