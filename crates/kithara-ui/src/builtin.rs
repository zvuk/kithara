use std::sync::LazyLock;

#[cfg(feature = "render")]
use crate::render::Skin;
use crate::{
    ids::SourceUri,
    skin::{SkinDoc, parse_skin},
    source::MemResolver,
    text::{TextDoc, parse_text},
};

pub const MICRO_PRESET: &str = "micro.klayout.ron";
pub const PLAYER_PRESET: &str = "player.klayout.ron";
pub const DARK_SKIN: &str = include_str!("../assets/kithara-dark.kskin.ron");
pub const TEXT_EN: &str = include_str!("../assets/kithara-en.ktext.ron");

#[must_use]
pub fn resolver() -> MemResolver {
    const ASSETS: &[(&str, &str)] = &[
        (MICRO_PRESET, include_str!("../assets/micro.klayout.ron")),
        (PLAYER_PRESET, include_str!("../assets/player.klayout.ron")),
        (
            "modules/deck-micro.kmodule.ron",
            include_str!("../assets/modules/deck-micro.kmodule.ron"),
        ),
        (
            "modules/global-bar.kmodule.ron",
            include_str!("../assets/modules/global-bar.kmodule.ron"),
        ),
        (
            "modules/deck.kmodule.ron",
            include_str!("../assets/modules/deck.kmodule.ron"),
        ),
        (
            "modules/deck/transport.kmodule.ron",
            include_str!("../assets/modules/deck/transport.kmodule.ron"),
        ),
        (
            "modules/deck/quality.kmodule.ron",
            include_str!("../assets/modules/deck/quality.kmodule.ron"),
        ),
        (
            "modules/deck/quality/auto.kmodule.ron",
            include_str!("../assets/modules/deck/quality/auto.kmodule.ron"),
        ),
        (
            "modules/deck/quality/row.kmodule.ron",
            include_str!("../assets/modules/deck/quality/row.kmodule.ron"),
        ),
        (
            "modules/library.kmodule.ron",
            include_str!("../assets/modules/library.kmodule.ron"),
        ),
    ];
    let mut resolver = MemResolver::default();
    for (path, text) in ASSETS {
        resolver.insert(path, text);
    }
    resolver
}

#[must_use]
pub fn skin_doc() -> &'static SkinDoc {
    static SKIN_DOC: LazyLock<SkinDoc> = LazyLock::new(|| {
        parse_skin(DARK_SKIN, &skin_origin())
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

#[cfg(feature = "render")]
#[must_use]
pub fn skin() -> &'static Skin {
    static SKIN: LazyLock<Skin> = LazyLock::new(|| {
        Skin::resolve(skin_doc().clone(), text_doc(), &skin_origin())
            .unwrap_or_else(|error| panic!("embedded kithara dark skin must resolve: {error}"))
    });
    &SKIN
}

fn skin_origin() -> SourceUri {
    SourceUri("builtin:kithara-dark.kskin.ron".to_owned())
}

fn text_origin() -> SourceUri {
    SourceUri("builtin:kithara-en.ktext.ron".to_owned())
}
