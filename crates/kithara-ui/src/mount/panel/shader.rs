use crate::{mount::Control, shader::ShaderSpec, size::SizeSpec, skin::SkinDoc};

/// A document-owned shader that occupies its declared layout box.
pub(crate) struct Shader<'a> {
    pub(crate) spec: &'a ShaderSpec,
}

impl<'a> Shader<'a> {
    pub(crate) const fn new(spec: &'a ShaderSpec) -> Self {
        Self { spec }
    }
}

impl Control for Shader<'_> {
    fn size(&self, _skin: &SkinDoc) -> SizeSpec {
        SizeSpec::FILL
    }
}
