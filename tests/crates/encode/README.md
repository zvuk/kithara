# kithara-encode-tests

Integration tests for the public `kithara::encode` facade live in
[`tests`](tests). The package keeps codec, stream, target, and error contracts
in one domain binary that unrelated test changes can leave cached.
