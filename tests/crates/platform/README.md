# Platform tests

Loom models verify facade-exposed mutexes, condition variables, channels,
atomics and flash wake gates. Run through `just test run --loom=on`; flash
models additionally require `--flash=on`.

See [test organization and execution](../../README.md).
