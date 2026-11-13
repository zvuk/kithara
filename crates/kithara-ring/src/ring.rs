use std::ops::DerefMut;

use ringbuf::{SharedRb, traits::Split};

use crate::storage::OwnedSlice;

/// Shared ring whose slots live in an owner's slice.
pub type Ring<B> = SharedRb<OwnedSlice<B>>;

/// Producer half of a [`Ring`].
pub type RingProd<B> = <Ring<B> as Split>::Prod;

/// Consumer half of a [`Ring`].
pub type RingCons<B> = <Ring<B> as Split>::Cons;

/// Producer and consumer halves returned by [`split`].
pub type RingHalves<B> = (RingProd<B>, RingCons<B>);

/// Split `owner` into the halves of a ring that uses its slice as slots.
///
/// The ring's capacity is the slice length, and every slot starts vacant.
///
/// # Errors
///
/// Returns the unchanged owner when its slice is empty.
pub fn split<B, T>(owner: B) -> Result<RingHalves<B>, B>
where
    B: DerefMut<Target = [T]>,
    T: Copy,
{
    let storage = OwnedSlice::new(owner)?;
    // SAFETY: equal indices mark every slot vacant, and `T: Copy` has no drop
    // glue, so the owner's existing values are overwritten, never dropped.
    let ring = unsafe { SharedRb::from_raw_parts(storage, 0, 0) };
    Ok(ring.split())
}

#[cfg(test)]
mod tests {
    use std::{
        ops::{Deref, DerefMut},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use kithara_test_utils::kithara;
    use ringbuf::traits::{Consumer, Observer, Producer};

    use super::split;

    struct Owned {
        drops: Arc<AtomicUsize>,
        slots: [i32; 4],
    }

    impl Deref for Owned {
        type Target = [i32];

        fn deref(&self) -> &[i32] {
            &self.slots
        }
    }

    impl DerefMut for Owned {
        fn deref_mut(&mut self) -> &mut [i32] {
            &mut self.slots
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[kithara::test(miri)]
    fn an_empty_owner_is_returned_unchanged() {
        let Err(owner) = split(Vec::<f32>::new()) else {
            panic!("an empty slice cannot hold a ring");
        };
        assert!(owner.is_empty());
    }

    #[kithara::test(miri)]
    fn the_ring_wraps_in_place_and_drops_its_owner_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = Owned {
            drops: Arc::clone(&drops),
            slots: [9; 4],
        };
        let Ok((mut prod, mut cons)) = split(owner) else {
            panic!("a non-empty owner forms a ring");
        };
        assert_eq!(prod.capacity().get(), 4);
        assert_eq!(cons.occupied_len(), 0, "existing values start vacant");

        assert_eq!(prod.push_slice(&[1, 2, 3]), 3);
        let mut first = [0; 2];
        assert_eq!(cons.pop_slice(&mut first), 2);
        assert_eq!(first, [1, 2]);
        assert_eq!(prod.push_slice(&[4, 5, 6, 7]), 3, "one slot stays taken");

        let mut rest = [0; 4];
        assert_eq!(cons.pop_slice(&mut rest), 4);
        assert_eq!(rest, [3, 4, 5, 6]);

        drop(prod);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "the consumer keeps it");
        drop(cons);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[kithara::test(miri)]
    fn the_halves_cross_threads() {
        let Ok((mut prod, mut cons)) = split(vec![0.0_f32; 8]) else {
            panic!("a non-empty owner forms a ring");
        };
        let writer = thread::spawn(move || {
            let mut sent = 0_u16;
            while sent < 1_000 {
                if prod.try_push(f32::from(sent)).is_ok() {
                    sent += 1;
                }
            }
        });
        let mut expected = 0_u16;
        while expected < 1_000 {
            if let Some(value) = cons.try_pop() {
                assert!((value - f32::from(expected)).abs() < f32::EPSILON);
                expected += 1;
            }
        }
        writer.join().expect("writer finishes");
    }
}
