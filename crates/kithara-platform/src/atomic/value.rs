use core::{fmt, marker::PhantomData, sync::atomic::Ordering};

#[cfg(not(all(feature = "loom", not(target_arch = "wasm32"))))]
use portable_atomic::{AtomicBool, AtomicU32, AtomicU64};

use super::order::{ReadOrder, Relaxed, WriteOrder};
#[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
use crate::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};

mod sealed {
    pub trait Sealed {}

    impl Sealed for bool {}
    impl Sealed for f32 {}
    impl Sealed for f64 {}
    impl Sealed for u32 {}
}

/// A primitive with an atomic storage representation.
pub trait AtomicPrimitive: Copy + sealed::Sealed {
    /// Backend-selected atomic storage.
    type Atomic;

    /// Loads a primitive value from the backing atomic.
    fn load(atomic: &Self::Atomic, order: Ordering) -> Self;

    /// Creates the backing atomic.
    fn new(value: Self) -> Self::Atomic;

    /// Stores a primitive value in the backing atomic.
    fn store(atomic: &Self::Atomic, value: Self, order: Ordering);
}

impl AtomicPrimitive for bool {
    type Atomic = AtomicBool;

    delegate::delegate! {
        to AtomicBool {
            fn load(atomic: &Self::Atomic, order: Ordering) -> Self;
            fn new(value: Self) -> Self::Atomic;
            fn store(atomic: &Self::Atomic, value: Self, order: Ordering);
        }
    }
}

impl AtomicPrimitive for u32 {
    type Atomic = AtomicU32;

    delegate::delegate! {
        to AtomicU32 {
            fn load(atomic: &Self::Atomic, order: Ordering) -> Self;
            fn new(value: Self) -> Self::Atomic;
            fn store(atomic: &Self::Atomic, value: Self, order: Ordering);
        }
    }
}

impl AtomicPrimitive for f32 {
    type Atomic = AtomicU32;

    delegate::delegate! {
        to AtomicU32 {
            #[expr(Self::from_bits($))]
            fn load(atomic: &Self::Atomic, order: Ordering) -> Self;
            #[expr(AtomicU32::new(value.to_bits()))]
            fn new(value: Self) -> Self::Atomic;
            #[expr(atomic.store(value.to_bits(), order))]
            fn store(atomic: &Self::Atomic, value: Self, order: Ordering);
        }
    }
}

impl AtomicPrimitive for f64 {
    type Atomic = AtomicU64;

    delegate::delegate! {
        to AtomicU64 {
            #[expr(Self::from_bits($))]
            fn load(atomic: &Self::Atomic, order: Ordering) -> Self;
            #[expr(AtomicU64::new(value.to_bits()))]
            fn new(value: Self) -> Self::Atomic;
            #[expr(atomic.store(value.to_bits(), order))]
            fn store(atomic: &Self::Atomic, value: Self, order: Ordering);
        }
    }
}

/// An atomic scalar with load and store orderings fixed by its type.
/// Cloning creates an independent snapshot, not another handle to the same atomic.
pub struct AtomicValue<T: AtomicPrimitive, R: ReadOrder, W: WriteOrder> {
    atomic: T::Atomic,
    _orders: PhantomData<(R, W)>,
}

impl<T: AtomicPrimitive, R: ReadOrder, W: WriteOrder> AtomicValue<T, R, W> {
    fn construct(value: T) -> Self {
        Self {
            atomic: T::new(value),
            _orders: PhantomData,
        }
    }

    /// Loads the current value with the type's read ordering.
    #[must_use]
    #[inline]
    pub fn load(&self) -> T {
        T::load(&self.atomic, R::ORDERING)
    }

    /// Stores a value with the type's write ordering.
    #[inline]
    pub fn store(&self, value: T) {
        T::store(&self.atomic, value, W::ORDERING);
    }
}

#[cfg(not(all(feature = "loom", not(target_arch = "wasm32"))))]
impl<R: ReadOrder, W: WriteOrder> AtomicValue<bool, R, W> {
    /// Creates a boolean atomic value.
    #[must_use]
    pub const fn new(value: bool) -> Self {
        Self {
            atomic: AtomicBool::new(value),
            _orders: PhantomData,
        }
    }
}

#[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
impl<R: ReadOrder, W: WriteOrder> AtomicValue<bool, R, W> {
    /// Creates a boolean atomic value inside a Loom model.
    #[must_use]
    pub fn new(value: bool) -> Self {
        Self::construct(value)
    }
}

#[cfg(not(all(feature = "loom", not(target_arch = "wasm32"))))]
impl<R: ReadOrder, W: WriteOrder> AtomicValue<f32, R, W> {
    /// Creates a floating-point atomic value.
    #[must_use]
    pub const fn new(value: f32) -> Self {
        Self {
            atomic: AtomicU32::new(value.to_bits()),
            _orders: PhantomData,
        }
    }
}

#[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
impl<R: ReadOrder, W: WriteOrder> AtomicValue<f32, R, W> {
    /// Creates a floating-point atomic value inside a Loom model.
    #[must_use]
    pub fn new(value: f32) -> Self {
        Self::construct(value)
    }
}

#[cfg(not(all(feature = "loom", not(target_arch = "wasm32"))))]
impl<R: ReadOrder, W: WriteOrder> AtomicValue<f64, R, W> {
    /// Creates a floating-point atomic value.
    #[must_use]
    pub const fn new(value: f64) -> Self {
        Self {
            atomic: AtomicU64::new(value.to_bits()),
            _orders: PhantomData,
        }
    }
}

#[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
impl<R: ReadOrder, W: WriteOrder> AtomicValue<f64, R, W> {
    /// Creates a floating-point atomic value inside a Loom model.
    #[must_use]
    pub fn new(value: f64) -> Self {
        Self::construct(value)
    }
}

#[cfg(not(all(feature = "loom", not(target_arch = "wasm32"))))]
impl<R: ReadOrder, W: WriteOrder> AtomicValue<u32, R, W> {
    /// Creates an unsigned atomic value.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self {
            atomic: AtomicU32::new(value),
            _orders: PhantomData,
        }
    }
}

#[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
impl<R: ReadOrder, W: WriteOrder> AtomicValue<u32, R, W> {
    /// Creates an unsigned atomic value inside a Loom model.
    #[must_use]
    pub fn new(value: u32) -> Self {
        Self::construct(value)
    }
}

impl<T: AtomicPrimitive, R: ReadOrder, W: WriteOrder> Clone for AtomicValue<T, R, W> {
    fn clone(&self) -> Self {
        Self::construct(self.load())
    }
}

impl<T: AtomicPrimitive + Default, R: ReadOrder, W: WriteOrder> Default for AtomicValue<T, R, W> {
    fn default() -> Self {
        Self::construct(T::default())
    }
}

impl<T: AtomicPrimitive + fmt::Debug, R: ReadOrder, W: WriteOrder> fmt::Debug
    for AtomicValue<T, R, W>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AtomicValue")
            .field(&self.load())
            .finish()
    }
}
/// Floating-point atomic value using relaxed loads and stores.
pub type RelaxedAtomicF32 = AtomicValue<f32, Relaxed, Relaxed>;

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    #[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
    use super::super::{Acquire, Release};
    use super::*;
    #[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
    use crate::{sync::Arc, thread};

    #[cfg(not(feature = "loom"))]
    #[kithara::test(native)]
    fn float_bits_and_clone_snapshot_are_preserved() {
        let value = RelaxedAtomicF32::new(f32::from_bits(0x7fc0_1234));
        let snapshot = value.clone();
        value.store(-0.0);
        assert_eq!(value.load().to_bits(), (-0.0_f32).to_bits());
        assert_eq!(snapshot.load().to_bits(), 0x7fc0_1234);

        let wide = AtomicValue::<f64, Relaxed, Relaxed>::new(f64::from_bits(0x7ff8_0000_0000_1234));
        let wide_snapshot = wide.clone();
        wide.store(-0.0);
        assert_eq!(wide.load().to_bits(), (-0.0_f64).to_bits());
        assert_eq!(wide_snapshot.load().to_bits(), 0x7ff8_0000_0000_1234);

        let count = AtomicValue::<u32, Relaxed, Relaxed>::new(7);
        let count_snapshot = count.clone();
        count.store(11);
        assert_eq!(count.load(), 11);
        assert_eq!(count_snapshot.load(), 7);
    }

    #[cfg(not(feature = "loom"))]
    #[kithara::test(native)]
    fn bool_clone_is_independent() {
        let value = AtomicValue::<bool, Relaxed, Relaxed>::new(false);
        let snapshot = value.clone();
        value.store(true);
        assert!(value.load());
        assert!(!snapshot.load());
    }

    #[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
    #[kithara::test(native, loom)]
    fn loom_models_bool_publication_and_scalar_values() {
        let value = Arc::new(RelaxedAtomicF32::new(0.0));
        let wide = Arc::new(AtomicValue::<f64, Relaxed, Relaxed>::new(0.0));
        let count = Arc::new(AtomicValue::<u32, Relaxed, Relaxed>::new(0));
        let ready = Arc::new(AtomicValue::<bool, Acquire, Release>::new(false));
        let writer_value = Arc::clone(&value);
        let writer_wide = Arc::clone(&wide);
        let writer_count = Arc::clone(&count);
        let writer_ready = Arc::clone(&ready);
        let writer = thread::spawn(move || {
            writer_value.store(f32::from_bits(0x7fc0_1234));
            writer_wide.store(f64::from_bits(0x7ff8_0000_0000_1234));
            writer_count.store(11);
            writer_ready.store(true);
        });

        if ready.load() {
            assert_eq!(value.load().to_bits(), 0x7fc0_1234);
            assert_eq!(wide.load().to_bits(), 0x7ff8_0000_0000_1234);
            assert_eq!(count.load(), 11);
        }
        writer.join().expect("model writer did not panic");
        assert!(ready.load());
        assert_eq!(value.load().to_bits(), 0x7fc0_1234);
        assert_eq!(wide.load().to_bits(), 0x7ff8_0000_0000_1234);
        assert_eq!(count.load(), 11);
        let snapshot = value.as_ref().clone();
        value.store(-0.0);
        assert_eq!(snapshot.load().to_bits(), 0x7fc0_1234);
    }
}
