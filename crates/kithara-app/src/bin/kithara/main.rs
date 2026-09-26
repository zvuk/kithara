#[cfg(not(feature = "gui"))]
compile_error!("`kithara` binary requires the `gui` feature");

#[cfg(not(target_arch = "wasm32"))]
mod desktop;
#[cfg(target_arch = "wasm32")]
mod web;

use kithara::platform::CancelToken;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> desktop::AppResult {
    desktop::main(CancelToken::root())
}

#[cfg(target_arch = "wasm32")]
fn main() {
    web::main(CancelToken::root());
}
