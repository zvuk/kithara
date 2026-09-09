# kithara-bufpool-tests

Integration tests for the public `kithara::bufpool` facade. The test sources
live in [`tests`](tests).

This package selects only the facade modules needed for pool allocation,
budgeting, reuse, and statistics tests. Keeping it separate prevents changes in
unrelated domains from rebuilding these test binaries.
