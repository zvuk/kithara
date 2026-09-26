# kithara-play-tests

Playback, seeking, buffering, and player lifecycle tests that need real media
or the offline harness live in [`tests`](tests). Regular, heavy, stress,
device, and network contracts use separate binaries so each execution surface
keeps an independent cached artifact.

Player, resource, engine, and RT processor contracts that drive
`kithara-play` alone are crate tests in `kithara-play`. Host mixing contracts
live in `kithara-host-tests`, warp tempo/pitch and rate-response contracts in
`kithara-warp-tests`, and synchronization contracts across Host, Player, and
Queue in `kithara-sync-tests`. Functional assertions determine ownership; the
test driver does not.
