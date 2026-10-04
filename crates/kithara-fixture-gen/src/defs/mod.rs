//! Each family gates itself at the head of its own file: a family that is off
//! registers nothing, so the build script has nothing to materialize.

mod encoded;
mod hls;
mod hls_inputs;
mod hls_variants;
mod library;
mod packaged;
mod pcm;
mod remote;
mod rhythm;
mod signal;
mod signal_profiles;

mod wav;
