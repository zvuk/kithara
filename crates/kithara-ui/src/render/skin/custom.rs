use std::collections::BTreeMap;

use crate::{
    draw::Rgba,
    error::UiDocError,
    ids::SourceUri,
    render::theme::{RenderPalette, color},
    skin::{CustomDoc, SettingDoc},
};

/// One value an extension reads from the skin, resolved.
///
/// A colour written as digits and a colour written as a palette role are the
/// same thing here: which of the two the skin wrote is settled while it
/// resolves, so an extension asking for a colour is never asked to resolve one.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Setting {
    Color(Rgba),
    Number(f32),
}

/// What one extension kind is dressed in.
///
/// An extension is content the toolkit does not own, so the skin cannot say
/// how it is drawn. It says what it is drawn with, name by name, and the
/// extension decides what to do with what it finds — including what to draw
/// when the skin dresses it in nothing.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct CustomSkin {
    settings: BTreeMap<String, Setting>,
}

impl CustomSkin {
    /// The dressing of a kind this skin says nothing about.
    pub(crate) const EMPTY: Self = Self {
        settings: BTreeMap::new(),
    };

    /// The colour this skin wrote under `name`.
    #[must_use]
    pub fn color(&self, name: &str) -> Option<Rgba> {
        match self.settings.get(name)? {
            Setting::Color(color) => Some(*color),
            Setting::Number(_) => None,
        }
    }

    /// The number this skin wrote under `name`.
    #[must_use]
    pub fn number(&self, name: &str) -> Option<f32> {
        match self.settings.get(name)? {
            Setting::Number(number) => Some(*number),
            Setting::Color(_) => None,
        }
    }
}

/// Every extension this skin dresses, by kind.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CustomSkins {
    kinds: BTreeMap<String, CustomSkin>,
}

impl CustomSkins {
    pub(crate) fn kind(&self, kind: &str) -> Option<&CustomSkin> {
        self.kinds.get(kind)
    }

    pub(crate) fn resolve(
        document: &CustomDoc,
        palette: &RenderPalette,
        origin: &SourceUri,
    ) -> Result<Self, UiDocError> {
        let mut kinds = BTreeMap::new();
        for (kind, declared) in &document.kinds {
            let mut settings = BTreeMap::new();
            for (name, setting) in &declared.settings {
                settings.insert(name.clone(), resolve(setting, palette, origin)?);
            }
            kinds.insert(kind.clone(), CustomSkin { settings });
        }
        Ok(Self { kinds })
    }
}

fn resolve(
    setting: &SettingDoc,
    palette: &RenderPalette,
    origin: &SourceUri,
) -> Result<Setting, UiDocError> {
    let resolved = match setting {
        SettingDoc::Color(written) => Setting::Color(color(written, origin)?),
        SettingDoc::Role(role) => Setting::Color(palette[*role]),
        SettingDoc::Number(number) => Setting::Number(*number),
    };
    Ok(resolved)
}
