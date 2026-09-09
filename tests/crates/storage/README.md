# kithara-storage-tests

Integration tests for the public `kithara::storage` facade. The test sources
live in [`tests`](tests).

This package selects only the facade modules and shared fixtures needed for
asset storage tests. Keeping it separate prevents changes in unrelated domains
from rebuilding these test binaries.
