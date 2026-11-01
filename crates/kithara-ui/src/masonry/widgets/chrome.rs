use super::mount::NodeLayout;
use crate::{
    atoms::chrome::{ChromeChevron, ChromeLabel, footer_role},
    draw::Rgba,
    expand::header_path,
    hosts::{
        event::control_event,
        solve::{Alignment, Length, Padding, Size},
    },
    layout::{Axis, FrameSides},
    masonry::retained::{
        MasonryHost, MasonryNode, Painted,
        flex::{ChildLayout, Flex},
        leaf::Leaf,
    },
    module::TextAlign,
    render::{ControlAction, document::Module},
    shaping::TextContext,
    skin::FrameSkin,
};

/// The frame a module wears: the bar across its top, the strip under it, and
/// the marks on both.
///
/// Its own file so the chrome does not drag the rest of the host over the size
/// gate, the way the leaf helpers already have their own block.
impl<Action> MasonryHost<'_, Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    /// A chrome frame is drawn only when the skin asks for one.
    fn chrome_frame(&self, frame: FrameSkin) -> Option<(FrameSides, Rgba, f32)> {
        (frame.border_width > 0.0).then(|| {
            (
                FrameSides::default(),
                self.skin.rgba(frame.border),
                frame.border_width,
            )
        })
    }

    /// A word in a box, sized by the word.
    fn chrome_label(&self, label: ChromeLabel, content: &str) -> MasonryNode<Action> {
        MasonryNode::control_leaf(
            Painted::pooled(
                label,
                content.to_owned(),
                self.skin,
                &self.ctx.ui.draw_buffers,
            ),
            Size::new(Length::Shrink, Length::Fill),
        )
    }

    /// The cell at the end of the header, with the mark that says which way
    /// the module folds.
    fn module_chevron(&self, collapsed: bool) -> MasonryNode<Action> {
        let metrics = self.skin.chrome;
        let declared = Size::new(Length::Fixed(metrics.chevron_size), Length::Fill);
        let mark = MasonryNode::control_leaf(
            Painted::pooled(
                ChromeChevron::new(self.skin),
                collapsed,
                self.skin,
                &self.ctx.ui.draw_buffers,
            ),
            declared,
        );
        MasonryNode::chrome(
            NodeLayout::Stack,
            declared,
            vec![mark],
            Some(self.skin.rgba(metrics.header_background)),
            self.chrome_frame(metrics.chevron_frame),
        )
    }

    /// The strip under a module, carrying the one word it resolved for itself.
    pub(crate) fn module_footer(&self, content: String) -> MasonryNode<Action> {
        let metrics = self.skin.chrome;
        let role = footer_role(self.skin);
        MasonryNode::chrome(
            NodeLayout::Leaf(Leaf::Text {
                align: TextAlign::Start,
                content,
                role,
                padding_x: metrics.footer_pad,
                color: self.skin.rgba(role.color),
                lit: None,
                text: Box::new(TextContext::from(self.skin.text_resources.as_ref())),
                room: f32::INFINITY,
            }),
            Size::new(Length::Fill, Length::Fixed(metrics.footer_height)),
            Vec::new(),
            Some(self.skin.rgba(metrics.footer_background)),
            self.chrome_frame(metrics.footer_frame),
        )
    }

    /// The bar across the top of a module: what it is called, what it is
    /// assigned to, and the chevron that folds it away.
    pub(crate) fn module_header(&self, module: &Module<'_>) -> MasonryNode<Action> {
        let metrics = self.skin.chrome;
        let mut children: Vec<MasonryNode<Action>> = Vec::with_capacity(4 + module.assign().len());
        if let Some(chip) = module.chip() {
            children
                .push(self.chrome_label(ChromeLabel::chip(self.skin), self.ctx.ui.resolve(chip)));
        }
        if let Some(title) = module.title() {
            children
                .push(self.chrome_label(ChromeLabel::title(self.skin), self.ctx.ui.resolve(title)));
            children.push(MasonryNode::furniture(
                NodeLayout::Leaf(Leaf::Empty),
                Size::new(Length::Fixed(metrics.inner_line_width), Length::Fill),
                Some(self.skin.rgba(metrics.inner_line)),
            ));
        }
        children.push(MasonryNode::empty(Size::new(Length::Fill, Length::Fill)));
        children.extend(module.assign().iter().map(|label| {
            self.chrome_label(ChromeLabel::chip(self.skin), self.ctx.ui.resolve(*label))
        }));
        children.push(self.module_chevron(module.collapsed()));
        let layouts = children
            .iter()
            .map(|child| ChildLayout::natural(child.natural(), None))
            .collect();
        let mut header = MasonryNode::chrome(
            NodeLayout::Flex(Flex::new(
                Axis::Horizontal,
                Length::Fill,
                Length::Fixed(metrics.header_height),
                Padding::default(),
                0.0,
                Alignment::Center,
                layouts,
            )),
            Size::new(Length::Fill, Length::Fixed(metrics.header_height)),
            children,
            Some(self.skin.rgba(metrics.header_background)),
            self.chrome_frame(metrics.header_frame),
        );
        let path = header_path(self.ctx.ui.resolve(module.instance()));
        header.set_actions(
            Some(self.event(move || control_event(&path, ControlAction::Activate))),
            None,
        );
        header
    }
}
