//! One shared object carrying the graph every test binary would otherwise link
//! statically. Linking it dynamically keeps that code in a single image.

pub use kithara;
#[cfg(target_os = "android")]
pub use kithara_app;
pub use kithara_integration_tests;
pub use kithara_test_fixtures;
pub use kithara_test_utils;
