use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// A square switch bound to one boolean endpoint.
pub(crate) struct Checkbox;

impl Control for Checkbox {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.checkbox.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Checkbox;
    use crate::{
        atoms::toggle::Binary,
        render::{
            ReadValue, Skin,
            controls::{Draws, Grip, Reading},
        },
    };

    impl Draws for Checkbox {
        type Painter = Binary;

        fn painter(&self, skin: &Skin) -> Binary {
            Binary::checkbox(skin)
        }

        fn data(&self, read: Reading<'_>) -> Option<bool> {
            match read.value {
                Some(ReadValue::Bool(active)) => Some(*active),
                _ => None,
            }
        }

        fn grip(&self, _skin: &Skin, _data: &bool) -> Grip {
            Grip::Press
        }
    }
}
