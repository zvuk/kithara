# kithara-hls-tests

Integration tests for the public `kithara::hls` facade live in [`tests`](tests).
Regular, heavy, and stress scenarios have separate binaries so edits outside
HLS do not invalidate their cached codegen and linking.
