use super::{controls::MasonryControl, custom::HostAction};
use crate::{
    atoms::{table::face::TableFace, tree::face::Tree as TreeFace},
    draw::{DrawList, Rect, Transform},
    interact::{Hit, Input, Outcome},
    render::{
        Skin,
        document::Ctx,
        hosted::{TablePlan, TreePlan},
    },
    shaping::TextContext,
};

pub(crate) type TableLeaf = ProjectedLeaf<TablePlan>;
pub(crate) type TreeLeaf = ProjectedLeaf<TreePlan>;

pub(crate) struct ProjectedLeaf<P> {
    plan: P,
    text: TextContext,
}

pub(crate) trait Projected {
    fn draw_list(&self, text: &mut TextContext, bounds: Rect) -> DrawList;
    fn refresh(&self, ctx: Ctx<'_, '_>) -> bool;
}

impl<P> ProjectedLeaf<P>
where
    P: Projected,
{
    pub(crate) fn new(plan: P, skin: &Skin) -> Self {
        Self {
            plan,
            text: TextContext::from(skin.text_resources()),
        }
    }
}

impl<P> MasonryControl for ProjectedLeaf<P>
where
    P: Projected,
{
    /// A projection hands back a list it already finished, so no pose reaches
    /// what it drew. The document refuses to put one on a `Table` or a `Tree`
    /// for exactly that reason, rather than moving the box and not the picture.
    fn draw_list(&mut self, bounds: Rect, _transform: Transform) -> DrawList {
        self.plan.draw_list(&mut self.text, bounds)
    }

    fn input(&mut self, _input: Input<'_>, _hit: &Hit) -> Outcome<HostAction> {
        Outcome::IGNORED
    }

    fn accepts_input(&self) -> bool {
        false
    }

    fn refresh(&mut self, ctx: Ctx<'_, '_>) -> bool {
        self.plan.refresh(ctx)
    }
}

impl Projected for TablePlan {
    fn draw_list(&self, text: &mut TextContext, bounds: Rect) -> DrawList {
        let Some(drawn) = self.drawn() else {
            return DrawList::default();
        };
        TableFace::commands(&self.picture(), text, bounds, &drawn)
    }

    fn refresh(&self, ctx: Ctx<'_, '_>) -> bool {
        self.refresh(ctx)
    }
}

impl Projected for TreePlan {
    fn draw_list(&self, text: &mut TextContext, bounds: Rect) -> DrawList {
        let Some(drawn) = self.drawn() else {
            return DrawList::default();
        };
        TreeFace::commands(&self.picture(), text, bounds, &drawn)
    }

    fn refresh(&self, ctx: Ctx<'_, '_>) -> bool {
        self.refresh(ctx)
    }
}
