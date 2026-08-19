use masonry::vello::Scene;

use crate::render::{shader::ShaderDeclaration, vis::VisDeclaration};

/// One complete retained frame: shader images, Vello commands, then native Vis draws.
#[non_exhaustive]
pub struct Frame {
    scene: Scene,
    shaders: Vec<ShaderDeclaration>,
    vis: Vec<VisDeclaration>,
}

impl Frame {
    pub(super) fn new(
        scene: Scene,
        shaders: Vec<ShaderDeclaration>,
        vis: Vec<VisDeclaration>,
    ) -> Self {
        Self {
            scene,
            shaders,
            vis,
        }
    }

    /// The Vello scene drawn before native effects.
    #[must_use]
    pub const fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Document shader images referenced by the Vello scene.
    #[must_use]
    pub fn shaders(&self) -> &[ShaderDeclaration] {
        &self.shaders
    }

    /// Native visualiser draws for the same frame, in logical window coordinates.
    #[must_use]
    pub fn vis(&self) -> &[VisDeclaration] {
        &self.vis
    }
}

impl From<Frame> for Scene {
    fn from(frame: Frame) -> Self {
        frame.scene
    }
}
