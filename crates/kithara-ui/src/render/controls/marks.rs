use std::cell::RefCell;

use iced::widget::canvas::Cache;

use crate::draw::{CachedValue, DrawList};

/// What a frame cost a control that keeps what it drew.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Marked {
    /// The key held: nothing was built and the geometry stands.
    Kept,
    /// The picture was built and came out the same: the geometry stands.
    Same,
    /// The picture came out different: the geometry was dropped.
    Changed,
}

/// A key asked about before it is built.
///
/// The implementor is the cheap likeness of a key: made of borrows, compared,
/// and dropped again. Only the frame that misses pays for a key the cache can
/// hold, which is what keeps a frame that changed nothing free of copying.
///
/// Both sides of the comparison are probes, so an implementor can derive its
/// equality rather than hand-write a field-by-field check that could quietly
/// stop reading one.
pub(crate) trait Probe {
    /// What the cache keeps when this probe misses.
    type Key: PartialEq;

    /// Whether the kept key is the one this probe stands for.
    fn holds(&self, key: &Self::Key) -> bool;

    /// This probe made into a key that outlives the frame.
    fn keep(self) -> Self::Key;
}

/// A key cheap enough to own is probed by reference.
impl<Key> Probe for &Key
where
    Key: Clone + PartialEq,
{
    type Key = Key;

    fn holds(&self, key: &Key) -> bool {
        *self == key
    }

    fn keep(self) -> Key {
        self.clone()
    }
}

/// One drawn picture kept beside the key it was built from, and the geometry
/// tessellated from that picture.
///
/// The key is what the drawing was made from, compared whole. A hash would be
/// cheaper to carry, but it would have to be taught every field the drawing
/// reads, and a field it was never taught freezes the control on the screen;
/// derived equality cannot forget one. The picture is compared as well, because
/// that answers the other question: a key that moved without changing the
/// drawing must not cost a tessellation.
pub(crate) struct Marks<Key>
where
    Key: PartialEq,
{
    geometry: Cache,
    kept: RefCell<CachedValue<Option<Key>, DrawList>>,
}

impl<Key> Default for Marks<Key>
where
    Key: PartialEq,
{
    fn default() -> Self {
        Self {
            geometry: crate::render::immediate::cache::canvas(),
            kept: RefCell::default(),
        }
    }
}

impl<Key> Marks<Key>
where
    Key: PartialEq,
{
    /// The kept picture and the geometry drawn from it, for as long as it takes
    /// to replay them. Nothing at all before the first [`Self::mark`].
    pub(crate) fn drawn<T>(&self, with: impl FnOnce(&Cache, &DrawList) -> T) -> Option<T> {
        let kept = self.kept.borrow();
        kept.value().map(|list| with(&self.geometry, list))
    }

    /// Builds the picture only when the key it hangs on moved, drops the kept
    /// geometry only when the picture that came out differs, and reports which
    /// of the two it had to do. Those answers are the whole of this cache, so
    /// they are returned rather than inferred from the pixels.
    ///
    /// The frame asks with a [`Probe`] and hands over a key only once it has
    /// missed: a frame that changed nothing must not copy what it drew from in
    /// order to find out that it did not.
    pub(crate) fn mark<Asked>(&self, probe: Asked, build: impl FnOnce() -> DrawList) -> Marked
    where
        Asked: Probe<Key = Key>,
    {
        let mut kept = self.kept.borrow_mut();
        if kept.value().is_some() && kept.key().as_ref().is_some_and(|key| probe.holds(key)) {
            return Marked::Kept;
        }
        let list = build();
        let changed = kept.value() != Some(&list);
        if changed {
            self.geometry.clear();
        }
        kept.update(Some(probe.keep()), Some(list));
        if changed {
            Marked::Changed
        } else {
            Marked::Same
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Marked, Marks, Probe};
    use crate::draw::{DrawList, DrawListBuilder, Rect, Rgba};

    /// One of two pictures a frame apart: the fill is all red, or none of it.
    fn filled(red: f32) -> DrawList {
        let mut builder = DrawListBuilder::default();
        builder.fill_rect(
            Rect {
                h: 10.0,
                w: 20.0,
                x: 0.0,
                y: 0.0,
            },
            Rgba {
                a: 1.0,
                b: 1.0 - red,
                g: 0.0,
                r: red,
            },
        );
        builder.finish()
    }

    #[kithara::test]
    fn the_first_frame_builds_its_picture() {
        let marks = Marks::default();

        assert_eq!(marks.mark(&1_u8, || filled(1.0)), Marked::Changed);
    }

    /// The whole point of the key: a control nothing touched must not be drawn
    /// from again, however cheap the drawing would have been.
    #[kithara::test]
    fn a_key_that_held_builds_nothing() {
        let marks = Marks::default();
        marks.mark(&1_u8, || filled(1.0));

        assert_eq!(
            marks.mark(&1, || panic!("a key that held must not build")),
            Marked::Kept
        );
    }

    /// A key the frame never has to own: a probe that stands for the kept key
    /// answers the question, and refuses to be turned into one.
    struct Costly(u8);

    impl Probe for Costly {
        type Key = u8;

        fn holds(&self, key: &u8) -> bool {
            self.0 == *key
        }

        fn keep(self) -> u8 {
            panic!("a key that held must not be paid for")
        }
    }

    /// The saving the probe exists for: a hit reads the kept key where it lies
    /// and copies nothing, however much the key would have cost to own.
    #[kithara::test]
    fn a_key_that_held_is_never_paid_for() {
        let marks = Marks::default();
        marks.mark(&1_u8, || filled(1.0));

        assert_eq!(marks.mark(Costly(1), || filled(1.0)), Marked::Kept);
    }

    /// The second stage answers what the key cannot: a key may move without the
    /// picture moving with it, and tessellating again for the same picture is
    /// the cost the list comparison exists to spare.
    #[kithara::test]
    fn a_key_that_moved_onto_the_same_picture_keeps_it() {
        let marks = Marks::default();
        marks.mark(&1_u8, || filled(1.0));

        assert_eq!(marks.mark(&2, || filled(1.0)), Marked::Same);
    }

    #[kithara::test]
    fn a_picture_that_moved_is_drawn_again() {
        let marks = Marks::default();
        marks.mark(&1_u8, || filled(1.0));

        assert_eq!(marks.mark(&2, || filled(0.0)), Marked::Changed);
    }

    /// A key that moved takes its picture with it, or the next frame would
    /// answer for a key it no longer holds.
    #[kithara::test]
    fn a_key_that_moved_is_the_one_the_next_frame_is_asked_about() {
        let marks = Marks::default();
        marks.mark(&1_u8, || filled(1.0));
        marks.mark(&2, || filled(0.0));

        assert_eq!(
            marks.mark(&2, || panic!(
                "the key that came with the picture must hold"
            )),
            Marked::Kept
        );
    }
}
