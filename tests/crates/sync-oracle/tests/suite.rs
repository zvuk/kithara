#![forbid(unsafe_code)]

use kithara_test_dylib as _;
use kithara_test_utils as _;

#[cfg(not(target_arch = "wasm32"))]
mod sync_oracle;
