use kithara_test_macros as kithara;

use crate::assets;

#[kithara::fixture]
#[must_use]
pub fn constant_half() -> &'static [u8] {
    assets::constant_track_half().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn constant_quarter() -> &'static [u8] {
    assets::constant_track_quarter().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn constant_quiet() -> &'static [u8] {
    assets::constant_track_quiet().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn constant_two() -> &'static [u8] {
    assets::constant_track_two().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn constant_three() -> &'static [u8] {
    assets::constant_track_three().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn constant_four() -> &'static [u8] {
    assets::constant_track_four().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn constant_loud() -> &'static [u8] {
    assets::constant_track_loud().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn constant_unity() -> &'static [u8] {
    assets::constant_track_unity().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn deadline_tracks() -> [&'static [u8]; 4] {
    [
        assets::deadline_block_one().bytes(),
        assets::deadline_block_two().bytes(),
        assets::deadline_block_three().bytes(),
        assets::deadline_block_four().bytes(),
    ]
}

#[kithara::fixture]
#[must_use]
pub fn benchmark_half() -> &'static [u8] {
    assets::benchmark_half_default().bytes()
}
