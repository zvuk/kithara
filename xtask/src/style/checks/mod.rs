//! Registry of code-style checks.
//!
//! Style checks operate on intra-file organisation (constant locality, field
//! and item ordering, init-expression form). They are independent from `arch`
//! topology checks: a single file can satisfy `arch` perfectly and still trip
//! `style` rules, and vice versa.

use std::path::Path;

use anyhow::Result;
use cargo_metadata::Metadata;

use super::config::StyleConfig;
use crate::common::violation::Violation;

pub(crate) mod const_locality;
pub(crate) mod struct_field_order;

#[expect(dead_code, reason = "fields consumed by upcoming style checks")]
pub(crate) struct Context<'a> {
    pub(crate) workspace_root: &'a Path,
    pub(crate) metadata: &'a Metadata,
    pub(crate) config: &'a StyleConfig,
}

pub(crate) trait Check {
    fn id(&self) -> &'static str;
    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>>;
}

pub(crate) fn registry() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(const_locality::ConstLocality),
        Box::new(struct_field_order::StructFieldOrder),
    ]
}
