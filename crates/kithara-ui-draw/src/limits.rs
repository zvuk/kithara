use bon::Builder;
use kithara_derive::Patch;

/// Memory retained by the draw pools between frames.
#[derive(Builder, Clone, Copy, Debug, PartialEq, Eq, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
#[derive(kithara_derive::BuiltDefault)]
pub struct DrawPoolLimits {
    /// Command slots retained by one returned draw-list buffer.
    #[builder(default = 512)]
    pub command_capacity: usize,
    /// Maximum reusable buffers kept by each pool. Zero is treated as one.
    #[builder(default = 64)]
    pub max_buffers: usize,
    /// Hard byte limit shared by every draw buffer kind.
    #[builder(default = 64 * 1024 * 1024)]
    pub max_bytes: usize,
    /// Vector verbs retained by one returned path buffer.
    #[builder(default = 128)]
    pub path_capacity: usize,
    /// UTF-8 bytes retained by one returned text buffer.
    #[builder(default = 128)]
    pub text_capacity: usize,
}
