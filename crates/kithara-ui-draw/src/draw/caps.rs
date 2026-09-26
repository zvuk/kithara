use super::{ir::DrawCmd, list::DrawList, style::Paint};

/// What a backend is able to draw.
///
/// Every backend states this once, as a constant. A backend that cannot do
/// something does not approximate it and does not skip it: the list that asks
/// is refused whole, before anything reaches the screen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Caps {
    /// Draws externally owned images.
    pub can_draw_images: bool,
    /// Scopes drawing to a rectangle.
    pub clip: bool,
    /// Ramps a fill along a line.
    pub linear_gradient: bool,
    /// Fills an outline that no named shape covers.
    pub outline: bool,
    /// Ramps a fill out from a centre.
    pub radial_gradient: bool,
}

impl Caps {
    /// A backend that draws everything the list can express.
    pub const EVERYTHING: Self = Self {
        clip: true,
        linear_gradient: true,
        outline: true,
        radial_gradient: true,
        can_draw_images: true,
    };

    /// Refuses a list this backend cannot draw whole.
    ///
    /// # Errors
    /// Returns the first capability the list needs and this backend lacks.
    pub const fn accepts(self, needs: Needs) -> Result<(), Unsupported> {
        if needs.clip && !self.clip {
            return Err(Unsupported::Clip);
        }
        if needs.outline && !self.outline {
            return Err(Unsupported::Outline);
        }
        if needs.linear_gradient && !self.linear_gradient {
            return Err(Unsupported::LinearGradient);
        }
        if needs.radial_gradient && !self.radial_gradient {
            return Err(Unsupported::RadialGradient);
        }
        if needs.images && !self.can_draw_images {
            return Err(Unsupported::Image);
        }
        Ok(())
    }
}

/// What a list needs its backend to be able to do.
///
/// Read off the list itself, so the question is answered by what is actually
/// drawn rather than by what a document might in principle contain.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Needs {
    pub(super) clip: bool,
    pub(super) images: bool,
    pub(super) linear_gradient: bool,
    pub(super) outline: bool,
    pub(super) radial_gradient: bool,
}

impl Needs {
    fn read(&mut self, list: &DrawList) {
        for command in list.commands() {
            match command {
                DrawCmd::Clip { list, .. } => {
                    self.clip = true;
                    self.read(list);
                }
                DrawCmd::Fill { geom, paint } => {
                    self.outline |= geom.is_outline();
                    self.linear_gradient |= matches!(paint, Paint::Linear { .. });
                    self.radial_gradient |= matches!(paint, Paint::Radial { .. });
                }
                DrawCmd::Image { .. } => self.images = true,
                DrawCmd::Stroke { geom, .. } => self.outline |= geom.is_outline(),
                DrawCmd::Text { .. } => {}
            }
        }
    }
}

impl From<&DrawList> for Needs {
    fn from(list: &DrawList) -> Self {
        let mut needs = Self::default();
        needs.read(list);
        needs
    }
}

/// What a list asked for that its backend cannot draw.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Unsupported {
    #[error("this backend cannot scope drawing to a clip")]
    Clip,
    #[error("this backend cannot ramp a fill along a line")]
    LinearGradient,
    #[error("this backend cannot fill an outline")]
    Outline,
    #[error("this backend cannot ramp a fill out from a centre")]
    RadialGradient,
    #[error("this backend cannot draw externally owned images")]
    Image,
}
