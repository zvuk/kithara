#[cfg(feature = "built-default")]
pub(crate) mod built;
#[cfg(feature = "patch")]
mod patch;

#[cfg(feature = "patch")]
pub(crate) use patch::expand;

#[cfg(feature = "config")]
pub(crate) mod retained;
