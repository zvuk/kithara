use anyhow::Result;

use super::{Check, Context, cancel_root_sites::configured_pattern_sites};
use crate::common::violation::Violation;

pub(crate) mod consts {
    pub(crate) const ID: &str = "smoothing_primitive_sites";

    pub(super) const EXPLANATION: &str = "\
Runtime parameters use firewheel `SmoothedParam`, composed as `MixDSP` for
A-to-B transitions. `SmoothingFilter` is sanctioned only for the equalizer's
biquad gain bank; move parameter smoothing to the owning config and primitive.";
}

pub(crate) struct SmoothingPrimitiveSites;

impl Check for SmoothingPrimitiveSites {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.smoothing_primitive_sites;
        configured_pattern_sites(ctx, &cfg.patterns, &cfg.allowed_files, &cfg.exempt_crates).map(
            |sites| {
                sites
                    .into_iter()
                    .map(|site| {
                        Violation::deny(
                        consts::ID,
                        site.location,
                        format!(
                            "hand-rolled parameter smoother `{}` outside the sanctioned gain bank",
                            site.pattern
                        ),
                    )
                    .with_explanation(consts::EXPLANATION)
                    })
                    .collect()
            },
        )
    }
}
