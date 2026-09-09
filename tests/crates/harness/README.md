# Harness tests

Tests of fixture audio artifacts, browser execution, the blocking detector,
flash rewriting and timeout enforcement.

`suite_light` participates in the ordinary workspace run; detector checks require
`no-block`. `suite_harness` requires `harness` and runs in the fixtures lane:
`just test run --lane=fixtures`. See [test organization](../../README.md).
