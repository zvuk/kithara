# kithara-net-tests

Integration tests for the public `kithara::net` facade live in [`tests`](tests).
The package keeps HTTP client, retry, and timeout contracts in one domain binary
that unrelated test changes can leave cached.
