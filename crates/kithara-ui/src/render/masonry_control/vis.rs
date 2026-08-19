use crate::render::{ReadValue, document::Ctx, vis::VisFrame};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct VisLeaf {
    #[field(get, vis = "pub(super)", copy)]
    frame: Option<VisFrame>,
    preset: Option<String>,
}

impl VisLeaf {
    pub(super) fn new(
        preset: Option<String>,
        value: Option<ReadValue<'_>>,
        ctx: Ctx<'_, '_>,
    ) -> Self {
        Self {
            frame: VisFrame::read(value, &ctx),
            preset,
        }
    }

    pub(super) fn refresh(&mut self, ctx: Ctx<'_, '_>) -> bool {
        let value = self.preset.as_deref().and_then(|preset| ctx.get(preset));
        let frame = VisFrame::read(value, &ctx);
        self.frame != frame && {
            self.frame = frame;
            true
        }
    }
}
