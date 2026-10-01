//! Property map

use crate::avm2::AvmString;
use crate::avm2::Multiname;
use crate::avm2::Namespace;
use crate::avm2::QName;
use fnv::FnvBuildHasher;
use gc_arena::{Collect, Gc, Mutation};
use smallvec::SmallVec;
use std::collections::HashMap;

/// Type which represents named properties on an object.
///
/// This type exposes interfaces akin to `HashMap<QName<'gc>, V>`, and is
/// intended to serve as a drop-in replacement optimized for objects where few
/// properties have overlapping local names. However, we have made slight
/// changes to the API in the following cases:
///
///  * Iterators return tuples of namespace, local-name, and value; rather than
///    a qualified name and value pair.
///  * Only `HashMap` methods and traits that we need are implemented.
///
/// The internal structure of the `PropertyMap` technically allows storage of
/// multiple values per `QName`. It's implementation enforces the invariant
/// that each `QName` only have one associated `V`.
#[derive(Clone, Debug, Collect)]
#[collect(no_drop)]
pub struct PropertyMap<'gc, V>(HashMap<AvmString<'gc>, Bucket<'gc, V>, FnvBuildHasher>);

type Bucket<'gc, V> = SmallVec<[(Namespace<'gc>, V); 2]>;

fn get_in<'a, 'gc, V>(bucket: Option<&'a Bucket<'gc, V>>, name: QName<'gc>) -> Option<&'a V> {
    bucket?
        .iter()
        .find(|(n, _)| n.matches_ns(name.namespace()))
        .map(|(_, v)| v)
}

fn get_for_multiname_in<'a, 'gc: 'a, V: 'a>(
    name: &Multiname<'gc>,
    bucket: impl FnOnce(AvmString<'gc>) -> Option<&'a Bucket<'gc, V>>,
) -> Option<&'a V> {
    if name.has_lazy_component() {
        unreachable!("Lookup on lazy Multiname should never happen ({:?})", name);
    }
    bucket(name.local_name()?)?
        .iter()
        .find(|(n, _)| name.namespace_set().iter().any(|ns| n.matches_ns(*ns)))
        .map(|(_, v)| v)
}

fn insert_in<'gc, V>(bucket: &mut Bucket<'gc, V>, name: QName<'gc>, value: V) -> Option<V> {
    let old_value = bucket
        .iter()
        .position(|(n, _)| n.matches_ns(name.namespace()))
        .map(|position| bucket.remove(position).1);
    bucket.insert(0, (name.namespace(), value));
    old_value
}

impl<V> Default for PropertyMap<'_, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'gc, V> PropertyMap<'gc, V> {
    pub fn new() -> Self {
        Self(Default::default())
    }

    fn bucket(&self, local_name: AvmString<'gc>) -> Option<&Bucket<'gc, V>> {
        self.0.get(&local_name)
    }

    pub fn get(&self, name: QName<'gc>) -> Option<&V> {
        get_in(self.bucket(name.local_name()), name)
    }

    pub fn get_for_multiname(&self, name: &Multiname<'gc>) -> Option<&V> {
        get_for_multiname_in(name, |local_name| self.bucket(local_name))
    }

    pub fn get_with_ns_for_multiname(&self, name: &Multiname<'gc>) -> Option<(Namespace<'gc>, &V)> {
        if name.has_lazy_component() {
            unreachable!("Lookup on lazy Multiname should never happen ({:?})", name);
        }
        if let Some(local_name) = name.local_name() {
            self.0.get(&local_name).iter().find_map(|v| {
                v.iter()
                    .filter(|(n, _)| name.namespace_set().iter().any(|ns| n.matches_ns(*ns)))
                    .map(|(ns, v)| (*ns, v))
                    .next()
            })
        } else {
            None
        }
    }

    pub fn get_mut(&mut self, name: QName<'gc>) -> Option<&mut V> {
        if let Some(bucket) = self.0.get_mut(&name.local_name())
            && let Some((_, old_value)) = bucket
                .iter_mut()
                .find(|(n, _)| n.matches_ns(name.namespace()))
        {
            return Some(old_value);
        }

        None
    }

    pub fn contains_key(&self, name: QName<'gc>) -> bool {
        self.0
            .get(&name.local_name())
            .iter()
            .any(|v| v.iter().any(|(n, _)| n.matches_ns(name.namespace())))
    }

    pub fn iter(&self) -> impl Iterator<Item = (AvmString<'gc>, Namespace<'gc>, &V)> {
        self.0
            .iter()
            .flat_map(|(k, vs)| vs.iter().map(|(ns, v)| (*k, *ns, v)))
    }

    pub fn insert(&mut self, name: QName<'gc>, value: V) -> Option<V> {
        insert_in(self.0.entry(name.local_name()).or_default(), name, value)
    }

    /// Preserves Flash Player ordering in scope caches and application domains.
    pub fn insert_at_end(&mut self, name: QName<'gc>, value: V) -> Option<V> {
        let bucket = self.0.entry(name.local_name()).or_default();

        if let Some((_, old_value)) = bucket
            .iter_mut()
            .find(|(n, _)| n.matches_ns(name.namespace()))
        {
            Some(std::mem::replace(old_value, value))
        } else {
            bucket.push((name.namespace(), value));
            None
        }
    }

    pub fn remove(&mut self, name: QName<'gc>) -> Option<V> {
        let bucket = self.0.get_mut(&name.local_name());

        if let Some(bucket) = bucket {
            let position = bucket
                .iter_mut()
                .enumerate()
                .find(|(_, (n, _))| n.matches_ns(name.namespace()));
            if let Some((position, _)) = position {
                return Some(bucket.remove(position).1);
            }
        }

        None
    }
}

/// A `PropertyMap` on top of a shared one, holding only the buckets it adds or
/// changes. A bucket here replaces the shared map's bucket for that local name,
/// so lookups find what they would in a copy of the shared map with the same
/// changes made to it.
#[derive(Collect)]
#[collect(no_drop)]
pub struct LayeredPropertyMap<'gc, V: 'gc> {
    own: PropertyMap<'gc, V>,
    base: Option<Gc<'gc, PropertyMap<'gc, V>>>,
    /// The local names in `own` that `base` doesn't have, in the order they
    /// were added: `flatten` adds them in that order, so that its map iterates
    /// in the order the copy would have.
    added: Vec<AvmString<'gc>>,
}

impl<V> Default for LayeredPropertyMap<'_, V> {
    fn default() -> Self {
        Self {
            own: PropertyMap::new(),
            base: None,
            added: Vec::new(),
        }
    }
}

impl<'gc, V: Clone + Collect<'gc>> LayeredPropertyMap<'gc, V> {
    pub fn new(base: Option<Gc<'gc, PropertyMap<'gc, V>>>) -> Self {
        Self {
            own: PropertyMap::new(),
            base,
            added: Vec::new(),
        }
    }

    fn bucket(&self, local_name: AvmString<'gc>) -> Option<&Bucket<'gc, V>> {
        if !self.own.0.is_empty()
            && let Some(bucket) = self.own.bucket(local_name)
        {
            return Some(bucket);
        }
        self.base.as_deref()?.bucket(local_name)
    }

    fn own_bucket(&mut self, local_name: AvmString<'gc>) -> &mut Bucket<'gc, V> {
        let Self { own, base, added } = self;
        own.0.entry(local_name).or_insert_with(|| {
            match base.as_deref().and_then(|base| base.bucket(local_name)) {
                Some(bucket) => bucket.clone(),
                None => {
                    added.push(local_name);
                    Bucket::new()
                }
            }
        })
    }

    pub fn get(&self, name: QName<'gc>) -> Option<&V> {
        get_in(self.bucket(name.local_name()), name)
    }

    pub fn get_for_multiname(&self, name: &Multiname<'gc>) -> Option<&V> {
        get_for_multiname_in(name, |local_name| self.bucket(local_name))
    }

    pub fn get_mut(&mut self, name: QName<'gc>) -> Option<&mut V> {
        self.get(name)?;
        self.own_bucket(name.local_name())
            .iter_mut()
            .find(|(n, _)| n.matches_ns(name.namespace()))
            .map(|(_, v)| v)
    }

    pub fn insert(&mut self, name: QName<'gc>, value: V) -> Option<V> {
        insert_in(self.own_bucket(name.local_name()), name, value)
    }

    pub fn flatten(&self, mc: &Mutation<'gc>) -> Gc<'gc, PropertyMap<'gc, V>> {
        if let Some(base) = self.base
            && self.own.0.is_empty()
        {
            return base;
        }
        let mut map = self
            .base
            .map_or_else(PropertyMap::new, |base| (*base).clone());
        for local_name in &self.added {
            map.0.insert(*local_name, self.own.0[local_name].clone());
        }
        for (local_name, bucket) in &self.own.0 {
            if let Some(flattened) = map.0.get_mut(local_name) {
                flattened.clone_from(bucket);
            }
        }
        Gc::new(mc, map)
    }
}
