use bon::Builder;

use crate::{
    ids::InternId,
    mount::Control,
    size::{Dim, SizeSpec},
    skin::SkinDoc,
};

/// A full-width tab heading one page of a panel.
#[derive(Builder)]
pub(crate) struct Tab {
    pub(crate) label: InternId,
}

impl Control for Tab {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        SizeSpec::new(Dim::Fill, Dim::Fixed(skin.tab_large.height))
    }

    /// A tab fills the strip it sits in, so a parent that measured it would
    /// size itself to a height the tab never asked for.
    fn composes_size(&self) -> bool {
        false
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Tab;
    use crate::{
        atoms::{painter::Labelled, tab::TabLarge},
        render::{
            ReadValue, Skin,
            controls::{Draws, Grip, Reading},
        },
    };

    impl Draws for Tab {
        type Painter = TabLarge;

        fn painter(&self, skin: &Skin) -> TabLarge {
            TabLarge::new(skin)
        }

        /// A tab heads a page, so one whose endpoint has not said whether its
        /// page is the current one draws nothing rather than a tab at rest.
        fn data(&self, read: Reading<'_>) -> Option<Labelled> {
            let Some(ReadValue::Bool(active)) = read.value else {
                return None;
            };
            Some(Labelled {
                active: *active,
                label: read.ui.resolve(self.label).to_owned(),
            })
        }

        fn grip(&self, _skin: &Skin, _data: &Labelled) -> Grip {
            Grip::Press
        }
    }
}
