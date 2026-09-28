//! Object representation for `flash.utils.Dictionary`

use crate::avm2::activation::Activation;
use crate::avm2::dynamic_map::DynamicKey;
use crate::avm2::object::script_object::ScriptObjectData;
use crate::avm2::object::{ClassObject, Object, TObject};
use crate::avm2::value::Value;
use crate::avm2::{Avm2, Error};
use crate::string::AvmString;
use core::fmt;
use gc_arena::{Collect, Gc, GcWeak, Mutation};
use ruffle_common::utils::HasPrefixField;
use std::cell::Cell;

/// A class instance allocator that allocates Dictionary objects.
pub fn dictionary_allocator<'gc>(
    class: ClassObject<'gc>,
    activation: &mut Activation<'_, 'gc>,
) -> Result<Object<'gc>, Error<'gc>> {
    let base = ScriptObjectData::new(class);

    Ok(DictionaryObject(Gc::new(
        activation.gc(),
        DictionaryObjectData {
            base,
            weak_keys: Cell::new(false),
            inserts_since_prune: Cell::new(0),
        },
    ))
    .into())
}

/// An object that allows associations between objects and values.
///
/// This is implemented by way of "object space", parallel to the property
/// space that ordinary properties live in. This space has no namespaces, and
/// keys are objects instead of strings.
#[derive(Clone, Collect, Copy)]
#[collect(no_drop)]
pub struct DictionaryObject<'gc>(pub Gc<'gc, DictionaryObjectData<'gc>>);

#[derive(Clone, Collect, Copy, Debug)]
#[collect(no_drop)]
pub struct DictionaryObjectWeak<'gc>(pub GcWeak<'gc, DictionaryObjectData<'gc>>);

impl fmt::Debug for DictionaryObject<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DictionaryObject")
            .field("ptr", &Gc::as_ptr(self.0))
            .finish()
    }
}

#[derive(Clone, Collect, HasPrefixField)]
#[collect(no_drop)]
#[repr(C, align(8))]
pub struct DictionaryObjectData<'gc> {
    /// Base script object
    base: ScriptObjectData<'gc>,

    #[collect(require_static)]
    weak_keys: Cell<bool>,

    #[collect(require_static)]
    inserts_since_prune: Cell<u32>,
}

const WEAK_PRUNE_INTERVAL: u32 = 64;

impl<'gc> DictionaryObject<'gc> {
    /// Retrieve a value in the dictionary's object space.
    pub fn get_property_by_object(self, name: Object<'gc>) -> Value<'gc> {
        self.base()
            .values()
            .get(&DynamicKey::Object(name))
            .map(|v| v.value)
            .unwrap_or(Value::Undefined)
    }

    /// Set a value in the dictionary's object space.
    pub fn set_property_by_object(self, name: Object<'gc>, value: Value<'gc>, mc: &Mutation<'gc>) {
        if !self.0.weak_keys.get() {
            self.base()
                .values_mut(mc)
                .insert(DynamicKey::Object(name), value);
            return;
        }

        let inserts = self.0.inserts_since_prune.get() + 1;
        let base = self.base();
        let mut values = base.values_mut(mc);
        if inserts >= WEAK_PRUNE_INTERVAL {
            values.prune_dead_keys();
            self.0.inserts_since_prune.set(0);
        } else {
            self.0.inserts_since_prune.set(inserts);
        }
        values.insert(DynamicKey::WeakObject(name.downgrade()), value);
    }

    pub fn set_weak_keys(self, avm2: &mut Avm2<'gc>) {
        if !self.0.weak_keys.replace(true) {
            avm2.register_weak_dictionary(self);
        }
    }

    /// Delete a value from the dictionary's object space.
    pub fn delete_property_by_object(self, name: Object<'gc>, mc: &Mutation<'gc>) {
        self.base().values_mut(mc).remove(&DynamicKey::Object(name));
    }

    pub fn has_property_by_object(self, name: Object<'gc>) -> bool {
        self.base().values().contains_key(&DynamicKey::Object(name))
    }
}

impl<'gc> TObject<'gc> for DictionaryObject<'gc> {
    fn gc_base(&self) -> Gc<'gc, ScriptObjectData<'gc>> {
        HasPrefixField::as_prefix_gc(self.0)
    }

    // Calling `setPropertyIsEnumerable` on a `Dictionary` has no effect -
    // stringified properties are always enumerable.
    fn set_local_property_is_enumerable(
        &self,
        _mc: &Mutation<'gc>,
        _name: AvmString<'gc>,
        _is_enumerable: bool,
    ) {
    }

    // Enumeration skips weak keys that have been dropped, but while the
    // collector sweeps, a dead key isn't dropped yet and already can't be
    // upgraded. Skip those too, or `for (k in dict)` would give `null`.
    fn get_next_enumerant(
        self,
        mut last_index: u32,
        activation: &mut Activation<'_, 'gc>,
    ) -> Result<u32, Error<'gc>> {
        let base = self.base();
        loop {
            let index = base.get_next_enumerant(last_index);
            let dying = matches!(
                base.values().key_at(index as usize),
                Some(DynamicKey::WeakObject(key)) if key.upgrade(activation.gc()).is_none()
            );
            if index == 0 || !dying {
                return Ok(index);
            }
            last_index = index;
        }
    }

    fn get_enumerant_value(
        self,
        index: u32,
        _activation: &mut Activation<'_, 'gc>,
    ) -> Result<Value<'gc>, Error<'gc>> {
        Ok(*self
            .base()
            .values()
            .value_at(index as usize)
            .unwrap_or(&Value::Undefined))
    }
}
