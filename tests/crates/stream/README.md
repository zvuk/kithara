# kithara-stream-tests

Integration tests for the public `kithara::stream` facade. The test sources
live in [`tests`](tests).

This package selects only the facade modules and in-memory source support needed
for stream tests. Keeping it separate prevents changes in unrelated domains
from rebuilding these test binaries.
