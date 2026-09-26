use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::palette::{ColorRole, parse_color};
use crate::{error::UiDocError, ids::SourceUri};

/// One value a skin writes for an extension it dresses.
///
/// The toolkit does not know what an extension draws, so it does not know what
/// any of these mean; it knows what kind of thing each is, which is enough to
/// resolve it once and hand it over typed.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[non_exhaustive]
pub enum SettingDoc {
    /// A colour of this skin's own, written the way the palette writes one.
    Color(String),
    /// A colour the palette already names, so an extension dressed in the
    /// accent follows whichever skin redefines the accent.
    Role(ColorRole),
    /// A plain number: a count, a radius, a proportion. What it counts is the
    /// extension's own business.
    Number(f32),
}

/// The settings one kind is dressed in, by name.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[non_exhaustive]
#[serde(transparent)]
pub struct KindDoc {
    pub settings: BTreeMap<String, SettingDoc>,
}

/// What a skin says about the extensions a document places, by kind.
///
/// An extension is content the toolkit does not own, so the skin cannot carry
/// a section for it the way it carries one for a fader. It carries a table
/// instead, and the extension reads its own name out of it: base settings for
/// every widget in the palette and the sections, and whatever this one is
/// dressed in here.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[non_exhaustive]
#[serde(transparent)]
pub struct CustomDoc {
    pub kinds: BTreeMap<String, KindDoc>,
}

/// What one skin restates of another skin's dressing.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[non_exhaustive]
#[serde(transparent)]
pub struct CustomPatch {
    pub kinds: BTreeMap<String, KindDoc>,
}

impl CustomDoc {
    /// Takes every setting the patch restates, keeping the rest.
    ///
    /// Two levels deep rather than one: a skin restating one colour of one
    /// extension keeps the rest of that extension's dressing, on the same
    /// terms a restated palette role keeps the other roles.
    pub(crate) fn patch(&mut self, patch: CustomPatch) {
        for (kind, restated) in patch.kinds {
            self.kinds
                .entry(kind)
                .or_default()
                .settings
                .extend(restated.settings);
        }
    }

    /// Reads every colour written here, so a skin that misspells one is
    /// refused where it was written rather than where it is drawn.
    pub(crate) fn validate(&self, origin: &SourceUri) -> Result<(), UiDocError> {
        for kind in self.kinds.values() {
            for setting in kind.settings.values() {
                if let SettingDoc::Color(written) = setting {
                    parse_color(written, origin)?;
                }
            }
        }
        Ok(())
    }
}
