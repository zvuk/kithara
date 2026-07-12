#[cfg(all(feature = "accelerate", any(target_os = "macos", target_os = "ios")))]
pub mod accelerate;
#[cfg(all(feature = "audio-toolbox", any(target_os = "macos", target_os = "ios")))]
pub mod audio_toolbox;
#[cfg(all(feature = "foundation", any(target_os = "macos", target_os = "ios")))]
pub mod foundation;
