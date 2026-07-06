#[cfg(all(target_arch = "wasm32", not(feature = "backend-web-audio")))]
compile_error!("kithara-play: wasm32 build requires `backend-web-audio`");

#[cfg(all(target_arch = "wasm32", not(feature = "wasm-bindgen")))]
compile_error!("kithara-play: wasm32 build requires `wasm-bindgen`");

#[cfg(all(not(target_arch = "wasm32"), not(feature = "backend-cpal")))]
compile_error!("kithara-play: non-wasm build requires `backend-cpal`");
