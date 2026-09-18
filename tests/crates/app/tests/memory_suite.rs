#![forbid(unsafe_code)]
//! Heap budget of the desktop application's composition root, one line per
//! subsystem.
//!
//! This binary installs its own counting allocator, so it is kept apart from
//! the `app` suite: every test in a binary shares its allocator, and the
//! measurements here only mean anything while nothing else allocates beside
//! them.

use kithara_test_dylib as _;

mod memory_budget;

#[global_allocator]
static HEAP: kithara_test_utils::memory::Counting = kithara_test_utils::memory::Counting;
