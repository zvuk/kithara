#![forbid(unsafe_code)]
//! Heap budget of the desktop application's composition root, one line per
//! subsystem.
//!
//! This binary installs its own counting allocator, keeping that overhead out
//! of the `app` suite. Measurements are local to the thread constructing each
//! subsystem, so unrelated runtime threads cannot enter its budget.

use kithara_test_dylib as _;

mod memory_budget;

#[global_allocator]
static HEAP: kithara_test_utils::memory::Counting = kithara_test_utils::memory::Counting;
