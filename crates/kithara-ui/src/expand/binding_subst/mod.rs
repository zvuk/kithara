mod binding;
#[cfg(test)]
mod tests;

pub use self::binding::scoped_key;
pub(crate) use self::binding::{
    intern_binding, resolve_optional_param, resolve_param, resolve_text_key, scoped_state,
    substitute_binding, substitute_map,
};
pub(super) use self::binding::{
    intern_module_text, intern_module_text_opt, intern_optional_binding, intern_optional_text,
    intern_text, intern_texts,
};
