mod doubles;
pub(crate) mod workspace;

pub(crate) use doubles::install_double;
#[cfg(unix)]
pub(crate) use doubles::{install_copy, install_script, install_scripted_double, signals};
