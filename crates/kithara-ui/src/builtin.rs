use crate::source::MemResolver;

pub const MICRO_PRESET: &str = "micro.klayout.ron";
pub const PLAYER_PRESET: &str = "player.klayout.ron";

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
        "modules/library.kmodule.ron",
        include_str!("../assets/modules/library.kmodule.ron"),
    ),
];

#[must_use]
pub fn resolver() -> MemResolver {
    let mut resolver = MemResolver::default();
    for (path, text) in ASSETS {
        resolver.insert(path, text);
    }
    resolver
}
