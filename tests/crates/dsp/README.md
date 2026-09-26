# kithara-dsp-tests

Integration tests for `kithara-dsp` kernels against the scalar helpers other
crates own: a kernel that promises to match `kithara-signal` is checked against
it here, where `kithara-dsp` cannot use it as an oracle. Kernel-to-oracle
parity inside the crate stays in `kithara-dsp`'s own tests. The `dsp` lane runs
both.
