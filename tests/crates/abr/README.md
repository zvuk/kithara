# kithara-abr-tests

Integration tests for the public `kithara::abr` facade live in [`tests`](tests).
The package keeps ABR state, concurrency, and switching contracts in their own
test binary so unrelated test changes can reuse its compiled artifact.
