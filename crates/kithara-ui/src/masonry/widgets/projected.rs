use crate::{
    atoms::{table::face::TableFace, tree::Tree as TreeFace},
    draw::{DrawList, Rect},
    hosts::hosted::{SearchPlan, TablePlan, TreePlan},
    interact::{Hit, Input, Outcome},
    masonry::{
        hosted::MasonryHostedState,
        paint::search::paint,
        retained::{controls::MasonryControl, custom::HostAction},
    },
    render::{Skin, document::Ctx},
    shaping::TextContext,
};

pub(crate) type TableLeaf = ProjectedLeaf<TablePlan<MasonryHostedState>>;
pub(crate) type SearchLeaf = ProjectedLeaf<SearchPlan<MasonryHostedState>>;
pub(crate) type TreeLeaf = ProjectedLeaf<TreePlan<MasonryHostedState>>;

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
            text: TextContext::from(skin.text_resources.as_ref()),
        }
    }
}

impl<P> MasonryControl for ProjectedLeaf<P>
where
    P: Projected,
{
    fn accepts_input(&self) -> bool {
        false
    }

    fn draw_list(&mut self, bounds: Rect) -> DrawList {
        self.plan.draw_list(&mut self.text, bounds)
    }

    fn input(&mut self, _input: Input<'_>, _hit: &Hit) -> Outcome<HostAction> {
        Outcome::IGNORED
    }

    fn refresh(&mut self, ctx: Ctx<'_, '_>) -> bool {
        self.plan.refresh(ctx)
    }
}

impl Projected for TablePlan<MasonryHostedState> {
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

impl Projected for TreePlan<MasonryHostedState> {
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

impl Projected for SearchPlan<MasonryHostedState> {
    fn draw_list(&self, text: &mut TextContext, bounds: Rect) -> DrawList {
        let Some(snapshot) = self.drawn() else {
            return DrawList::default();
        };
        let mut list = crate::draw::DrawListBuilder::default();
        let picture = self.picture.borrow();
        paint(
            &mut list,
            text,
            bounds,
            picture.query(),
            picture.skin(),
            &snapshot,
        );
        list.finish()
    }

    fn refresh(&self, ctx: Ctx<'_, '_>) -> bool {
        self.refresh(ctx)
    }
}
