// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! Blanket `ErasedItem` impl for any native item type.
//!
//! Items are stored as `Pin<Rc<dyn ErasedItem>>`.
//! Trait methods look up `PropertyInfo`/`CallbackInfo` by name via `BuiltinItem`.

use crate::Value;
use crate::erased::{ErasedItem, ErasedItemRc};
use i_slint_core::Property;
use i_slint_core::items::{ItemVTable, PropertyAnimation};
use i_slint_core::rtti::{
    AnimatedBindingKind, BuiltinItem, CallbackInfo, PropertyInfo, TwoWayBindingMapping,
};
use std::pin::Pin;
use std::rc::Rc;
use vtable::{HasStaticVTable, VRef};

/// Telescope patch: the property and callback tables of an item type, built once per thread and type, owned by the thread.
///
/// `BuiltinItem::properties` builds a fresh Vec of every property of the type on each call, and the interpreter looked a
/// property up by name on every native read, write and callback call, so each one allocated and freed that Vec.
struct Tables<T: 'static> {
    properties: Vec<(&'static str, &'static dyn PropertyInfo<T, Value>)>,
    callbacks: Vec<(&'static str, &'static dyn CallbackInfo<T, Value>)>,
}

/// Hashes a `TypeId`, which is already a hash.
#[derive(Default)]
struct IdHasher(u64);

impl std::hash::Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.0 ^= u64::from_le_bytes(word);
        }
    }
}

thread_local! {
    static TABLES: std::cell::RefCell<
        std::collections::HashMap<
            std::any::TypeId,
            Box<dyn std::any::Any>,
            std::hash::BuildHasherDefault<IdHasher>,
        >,
    > = Default::default();
}

/// Runs `find` on the tables of `T`, `None` when the thread's tables are already gone (a thread-local destructor reading
/// a property while the thread ends).
fn with_tables<T: 'static + BuiltinItem, R>(find: impl FnOnce(&Tables<T>) -> R) -> Option<R> {
    TABLES
        .try_with(|tables| {
            let mut tables = tables.borrow_mut();
            let entry = tables.entry(std::any::TypeId::of::<T>()).or_insert_with(|| {
                Box::new(Tables::<T> {
                    properties: T::properties::<Value>(),
                    callbacks: T::callbacks::<Value>(),
                })
            });
            find(entry.downcast_ref::<Tables<T>>().expect("tables of another type"))
        })
        .ok()
}

fn find_property<T: 'static + BuiltinItem>(
    name: &str,
) -> Option<&'static dyn PropertyInfo<T, Value>> {
    let find = |properties: &[(&'static str, &'static dyn PropertyInfo<T, Value>)]| {
        properties.iter().find_map(|(n, info)| (*n == name).then_some(*info))
    };
    match with_tables::<T, _>(|tables| find(&tables.properties)) {
        Some(found) => found,
        None => find(&T::properties::<Value>()),
    }
}

fn find_callback<T: 'static + BuiltinItem>(
    name: &str,
) -> Option<&'static dyn CallbackInfo<T, Value>> {
    let find = |callbacks: &[(&'static str, &'static dyn CallbackInfo<T, Value>)]| {
        callbacks.iter().find_map(|(n, info)| (*n == name).then_some(*info))
    };
    match with_tables::<T, _>(|tables| find(&tables.callbacks)) {
        Some(found) => found,
        None => find(&T::callbacks::<Value>()),
    }
}

impl<T> ErasedItem for T
where
    T: 'static + BuiltinItem + HasStaticVTable<ItemVTable>,
{
    fn get_property(self: Pin<&Self>, name: &str) -> Result<Value, ()> {
        find_property::<T>(name).ok_or(())?.get(self)
    }

    fn set_property(
        self: Pin<&Self>,
        name: &str,
        value: Value,
        animation: Option<PropertyAnimation>,
    ) -> Result<(), ()> {
        find_property::<T>(name).ok_or(())?.set(self, value, animation)
    }

    fn set_property_binding(
        self: Pin<&Self>,
        name: &str,
        binding: Box<dyn Fn() -> Value>,
        animation: AnimatedBindingKind,
    ) -> Result<(), ()> {
        find_property::<T>(name).ok_or(())?.set_binding(self, binding, animation)
    }

    fn prepare_property_for_two_way_binding(
        self: Pin<&Self>,
        name: &str,
    ) -> Option<Pin<Rc<Property<Value>>>> {
        Some(find_property::<T>(name)?.prepare_for_two_way_binding(self))
    }

    fn link_property_two_way_with_map(
        self: Pin<&Self>,
        name: &str,
        other: Pin<Rc<Property<Value>>>,
        mapper: Option<Rc<dyn TwoWayBindingMapping<Value>>>,
    ) {
        if let Some(info) = find_property::<T>(name) {
            info.link_two_way_with_map(self, other, mapper);
        }
    }

    fn call_callback(self: Pin<&Self>, name: &str, args: &[Value]) -> Result<Value, ()> {
        find_callback::<T>(name).ok_or(())?.call(self, args)
    }

    fn set_callback_handler(
        self: Pin<&Self>,
        name: &str,
        handler: Box<dyn Fn(&[Value]) -> Value>,
    ) -> Result<(), ()> {
        find_callback::<T>(name).ok_or(())?.set_handler(self, handler)
    }

    fn set_field(self: Pin<&Self>, _name: &str, _value: Value) -> Result<(), ()> {
        // `FieldInfo::set_field` needs `&mut T`, which a `Pin<&Self>` can't give.
        // Layouts write geometric fields via the corresponding `Property<Length>`,
        // so this stays unused until a concrete caller forces the design.
        Err(())
    }

    fn as_item_ref(self: Pin<&Self>) -> Pin<VRef<'_, ItemVTable>> {
        VRef::new_pin(self)
    }
}

/// Allocate a default-constructed item of type `T` as a boxed `ErasedItemRc`.
pub fn make_item<T>() -> ErasedItemRc
where
    T: 'static + Default + BuiltinItem + HasStaticVTable<ItemVTable>,
{
    Rc::pin(T::default())
}
