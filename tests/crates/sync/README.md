# kithara-sync-tests

Synchronization-fixture validation in [tests](tests): static PCM rhythm oracles
and a census that materializes every provider's file and HLS sources. Provider
support does not construct a playback engine. Runtime, product-matrix, listening,
and staging acceptance are outside this package's restored coverage.

Run the suite through `just test run -p kithara-sync-tests --test sync`.
Plain playback, source-rate conversion, and seek continuity remain in
`kithara-play-tests`.
