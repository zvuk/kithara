use bon::Builder;

use crate::{module::ScalarFormat, mount::Control, size::SizeSpec, skin::SkinDoc};

/// One formatted number read from an endpoint.
#[derive(Builder)]
pub(crate) struct Telemetry {
    pub(crate) format: ScalarFormat,
    pub(crate) framed: bool,
}

impl Control for Telemetry {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.telemetry.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Telemetry;
    use crate::{
        atoms::label::telemetry::Telemetry as Face,
        render::{
            ReadValue, Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for Telemetry {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(self.format, self.framed, skin)
        }

        /// A reading is the number its endpoint reports, so one with no number
        /// draws nothing rather than a zero nobody measured.
        fn data(&self, read: Reading<'_>) -> Option<f64> {
            match read.value {
                Some(ReadValue::Scalar(value)) => Some(*value),
                _ => None,
            }
        }
    }
}
