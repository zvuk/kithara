# kithara-decode-tests

Integration tests for the public `kithara::decode` facade live in
[`tests`](tests). Regular and fixture-heavy decoder contracts have separate
binaries so unrelated domains do not invalidate their cached codegen.
