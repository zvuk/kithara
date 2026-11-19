mod core;
#[cfg(test)]
mod tests;

pub(crate) use self::core::Cascade;
#[cfg(test)]
pub(super) use self::core::cascade_kernel;
