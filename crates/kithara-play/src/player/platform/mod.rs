mod load;

#[cfg_attr(target_arch = "wasm32", path = "wasm.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "native.rs")]
mod imp;

pub(crate) use imp::{
    PreparedBindingResource, activate_load, prepare_bound_load, restore_prepared_binding,
};
pub(crate) use load::ItemLoadContext;
