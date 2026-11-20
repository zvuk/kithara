use std::sync::LazyLock;

#[cfg(feature = "render")]
use crate::render::Skin;
use crate::{
    ids::SourceUri,
    skin::{ColorRole, SkinDoc, load_skin},
    source::{Limits, MemResolver},
    text::{TextDoc, parse_text},
};

include!(concat!(env!("OUT_DIR"), "/builtin_assets.rs"));

/// Minor-key palette tokens in Open Key wheel order.
pub const MINOR_KEY_COLORS: [ColorRole; 12] = [
    ColorRole::Key1m,
    ColorRole::Key2m,
    ColorRole::Key3m,
    ColorRole::Key4m,
    ColorRole::Key5m,
    ColorRole::Key6m,
    ColorRole::Key7m,
    ColorRole::Key8m,
    ColorRole::Key9m,
    ColorRole::Key10m,
    ColorRole::Key11m,
    ColorRole::Key12m,
];
/// Major-key palette tokens in Open Key wheel order.
pub const MAJOR_KEY_COLORS: [ColorRole; 12] = [
    ColorRole::Key1d,
    ColorRole::Key2d,
    ColorRole::Key3d,
    ColorRole::Key4d,
    ColorRole::Key5d,
    ColorRole::Key6d,
    ColorRole::Key7d,
    ColorRole::Key8d,
    ColorRole::Key9d,
    ColorRole::Key10d,
    ColorRole::Key11d,
    ColorRole::Key12d,
];

pub const MICRO_PRESET: &str = "micro.klayout.ron";
pub const PLAYER_PRESET: &str = "player.klayout.ron";
pub const DARK_SKIN_PATH: &str = "kithara-dark.kskin.ron";
/// Every skin this crate ships, in the order a picker offers them. The first is
/// the one a host wears when it names none.
///
/// Paper, neon and soft are written over the dark one. Paper restates its
/// palette and nothing else; neon and soft restate measurements and frames as
/// well, which is where a skin stops being a colour scheme.
pub const SKIN_PATHS: [&str; 4] = [
    DARK_SKIN_PATH,
    "kithara-light.kskin.ron",
    "kithara-neon.kskin.ron",
    "kithara-soft.kskin.ron",
];
pub const TEXT_EN: &str = include_str!("../assets/kithara-en.ktext.ron");

/// The shipped folder, resolved from memory, for a host with no checkout to
/// read it from.
#[must_use]
pub fn resolver() -> MemResolver {
    let mut resolver = MemResolver::default();
    for (path, text) in ASSETS {
        resolver.insert(path, text);
    }
    for (path, bytes) in PICTURES {
        resolver.insert_bytes(path, bytes);
    }
    resolver
}

#[must_use]
pub fn skin_doc() -> &'static SkinDoc {
    static SKIN_DOC: LazyLock<SkinDoc> = LazyLock::new(|| {
        load_skin(&resolver(), DARK_SKIN_PATH, &Limits::default())
            .unwrap_or_else(|error| panic!("embedded kithara dark skin must be valid: {error}"))
    });
    &SKIN_DOC
}

#[must_use]
pub fn text_doc() -> &'static TextDoc {
    static TEXT_DOC: LazyLock<TextDoc> = LazyLock::new(|| {
        parse_text(TEXT_EN, &text_origin())
            .unwrap_or_else(|error| panic!("embedded kithara text catalog must be valid: {error}"))
    });
    &TEXT_DOC
}

/// The skin a host wears when it names none.
#[cfg(feature = "render")]
#[must_use]
pub fn skin() -> &'static Skin {
    &skins()[0]
}

/// Every shipped skin, resolved, in the order [`SKIN_PATHS`] declares.
#[cfg(feature = "render")]
#[must_use]
pub fn skins() -> &'static [Skin] {
    static SKINS: LazyLock<Vec<Skin>> = LazyLock::new(|| {
        let resolver = resolver();
        SKIN_PATHS
            .iter()
            .map(|path| {
                let origin = SourceUri((*path).to_owned());
                let document = load_skin(&resolver, path, &Limits::default())
                    .unwrap_or_else(|error| panic!("embedded skin {path} must be valid: {error}"));
                Skin::resolve(document, text_doc(), &origin, &resolver)
                    .unwrap_or_else(|error| panic!("embedded skin {path} must resolve: {error}"))
            })
            .collect()
    });
    &SKINS
}

fn text_origin() -> SourceUri {
    SourceUri("builtin:kithara-en.ktext.ron".to_owned())
}
