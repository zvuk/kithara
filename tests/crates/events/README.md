# Kithara events tests

Integration tests owned by `kithara-events`.

This package keeps event-bus behavior separate from the cross-domain integration
suites. Changes to event tests now rebuild and link this small test binary
instead of the former workspace-wide aggregate. Shared test macros still come
from the existing `kithara-test-utils` package.
