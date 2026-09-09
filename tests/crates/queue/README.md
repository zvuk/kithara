# kithara-queue-tests

Integration tests for the public `kithara::queue` facade live in [`tests`](tests).
Regular and network-bound queue contracts use separate binaries so changes in
other domains do not invalidate their compiled artifacts.
