use super::{Grip, IndexEvent};
use crate::{
    atoms::painter::ControlPainter,
    render::{ReadValue, Skin, document::Ctx},
};

/// A control that draws itself: one painter, and the value it paints.
///
/// Declared once, in the control's own file, so what a control looks like
/// cannot differ between the two hosts by construction. Each host mounts it
/// through its own adapter and adds nothing to the picture.
pub(crate) trait Draws {
    type Painter: ControlPainter;

    /// The painter, with the skin already resolved into it.
    fn painter(&self, skin: &Skin) -> Self::Painter;

    /// What it draws this frame, or nothing at all when its endpoint has not
    /// said yet - an unbound switch is an empty box, not an idle switch.
    fn data(&self, read: Reading<'_>) -> Option<<Self::Painter as ControlPainter>::Data>;

    /// What the pointer means to it where the document says the leaf owns input.
    fn grip(&self, _skin: &Skin, _data: &<Self::Painter as ControlPainter>::Data) -> Grip {
        Grip::None
    }

    fn index_event(&self) -> Option<IndexEvent<<Self::Painter as ControlPainter>::Data>> {
        None
    }

    /// How a leaf that is mounted once steps itself afterwards, given the
    /// endpoint its reading came from.
    ///
    /// The name is passed rather than kept on the reading because only a
    /// retained host has anything to do with it: an immediate one asks the
    /// document again every frame and never holds a leaf across two.
    #[cfg(feature = "masonry")]
    fn retained_refresh(
        &self,
        _read: Reading<'_>,
        _endpoint: Option<&str>,
    ) -> Option<DataRefresh<<Self::Painter as ControlPainter>::Data>> {
        None
    }
}

#[cfg(feature = "masonry")]
pub(crate) type DataRefresh<Data> = Box<dyn Fn(&mut Data, Ctx<'_, '_>) -> bool>;

/// What a control is handed when it decides what to draw.
///
/// Values are reached through [`Ctx`] and nowhere else, so a control cannot be
/// written that quietly misses what the host answers for itself — the frame's
/// own clock among it.
#[derive(Clone, Copy)]
pub(crate) struct Reading<'a> {
    pub(crate) ctx: Ctx<'a, 'a>,
    pub(crate) scope: &'a str,
    pub(crate) value: Option<&'a ReadValue<'a>>,
}
