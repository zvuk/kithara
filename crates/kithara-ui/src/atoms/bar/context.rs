use crate::{
    atoms::{
        design::{picker::Picker, quad::center_y},
        icon::mark::Marked,
    },
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::{Mark, Skin},
    shaping::TextContext,
    skin::{FontFamily, TextRoleSkin},
};

/// The chevron between the scope and the path it opens onto.
const SEPARATOR: &str = "\u{203a}";

/// The strip under the tree: what is in view, and — when the document offers
/// more than one — which scope it is in view of.
pub(crate) struct Context {
    breadcrumb: Rgba,
    breadcrumb_role: TextRoleSkin,
    background: Rgba,
    content_height: f32,
    divider: Rgba,
    divider_width: f32,
    gap: f32,
    icon: Option<Mark>,
    icon_color: Rgba,
    icon_size: f32,
    padding_x: f32,
    picker: Picker,
    separator: Rgba,
    separator_role: TextRoleSkin,
    scope_gap: f32,
}

/// What the strip is handed each frame: the path in view, and the scope picker
/// beside it when the document declared one.
pub(crate) struct Viewed {
    pub(crate) breadcrumb: String,
    pub(crate) scope: Option<Scope>,
}

/// The words a scope picker offers, and which of them is picked.
pub(crate) struct Scope {
    pub(crate) items: Vec<String>,
    pub(crate) selected: Option<usize>,
}

impl Context {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = &skin.tree;
        let mono = |font: crate::skin::FontSkin, color| TextRoleSkin {
            color,
            font: FontFamily::Mono,
            size: font.size,
            spacing: 0.0,
            weight: font.weight,
        };
        Self {
            background: skin.rgba(metrics.context_background),
            breadcrumb: skin.palette.text_dim,
            breadcrumb_role: mono(metrics.context_text, metrics.scope_text_color),
            content_height: metrics.context_height - metrics.context_divider_width,
            divider: skin.rgba(metrics.context_divider),
            divider_width: metrics.context_divider_width,
            gap: metrics.context_gap,
            // Decoration rather than content: the path is what this strip says,
            // and a mark that cannot be read leaves the words where they are.
            icon: crate::render::Icon::Zvuk.mark(),
            icon_color: skin.palette.text,
            icon_size: metrics.context_icon_size,
            padding_x: metrics.context_padding_x,
            picker: Picker::new(skin),
            scope_gap: metrics.scope_gap,
            separator: skin.rgba(metrics.scope_chevron_color),
            separator_role: mono(metrics.scope_text, metrics.scope_chevron_color),
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Viewed,
        bounds: Rect,
    ) {
        let content = Rect {
            h: self.content_height.min(bounds.h),
            ..bounds
        };
        list.fill_rect(content, self.background);
        let gap = if data.scope.is_some() {
            self.scope_gap
        } else {
            self.gap
        };
        let mut x = content.x + self.padding_x;
        if let Some(icon) = self.icon {
            x += Marked::new(icon, self.icon_size).paint(list, text, x, content, self.icon_color)
                + gap;
        }
        if let Some(scope) = &data.scope {
            let face = self
                .face(text, data)
                .map_or(content, |face| Self::placed(face, content));
            self.picker.face(
                list,
                text,
                scope
                    .selected
                    .and_then(|index| scope.items.get(index))
                    .map(String::as_str),
                face,
            );
            x += face.w + gap;
            x += word(
                list,
                text,
                SEPARATOR,
                self.separator_role,
                self.separator,
                x,
                content,
            ) + gap;
        }
        word(
            list,
            text,
            &data.breadcrumb,
            self.breadcrumb_role,
            self.breadcrumb,
            x,
            content,
        );
        list.fill_rect(
            Rect {
                h: self.divider_width,
                w: bounds.w,
                x: bounds.x,
                y: content.y + content.h,
            },
            self.divider,
        );
    }

    /// Where in the strip the scope's face sits, when the document declared a
    /// scope at all.
    ///
    /// The menu a host opens hangs off this box, not off the strip, so the
    /// placement is answered here rather than measured a second time by
    /// whoever raises the menu.
    pub(crate) fn face(&self, text: &mut TextContext, data: &Viewed) -> Option<Rect> {
        let scope = data.scope.as_ref()?;
        Some(self.face_of(text, scope.items.iter().map(String::as_str)))
    }

    /// The same box for a scope named rather than read: what an engine plan
    /// knows about a strip before anything is drawn.
    pub(crate) fn face_of<'a>(
        &self,
        text: &mut TextContext,
        items: impl IntoIterator<Item = &'a str>,
    ) -> Rect {
        let icon = self.icon.map_or(0.0, |icon| {
            Marked::new(icon, self.icon_size).width(text) + self.scope_gap
        });
        Rect {
            h: self.content_height,
            w: self.picker.width(text, items),
            x: self.padding_x + icon,
            y: 0.0,
        }
    }

    /// The same box where the strip actually landed.
    pub(crate) const fn placed(face: Rect, bounds: Rect) -> Rect {
        Rect {
            x: bounds.x + face.x,
            y: bounds.y + face.y,
            ..face
        }
    }
}

/// One word placed across the strip at `x` and centred down it, answering how
/// much room it took.
fn word(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    content: &str,
    role: TextRoleSkin,
    color: Rgba,
    x: f32,
    bounds: Rect,
) -> f32 {
    let run = text.shape(content, role, None);
    list.text(
        &run,
        content,
        Transform::translate(Pt {
            x,
            y: center_y(bounds, &run),
        }),
        color,
    );
    run.width()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Context, DrawListBuilder, Rect, SEPARATOR, Scope, TextContext, Viewed};
    use crate::{builtin, draw::DrawCmd};

    const BOUNDS: Rect = Rect {
        h: 30.0,
        w: 280.0,
        x: 0.0,
        y: 0.0,
    };

    fn drawn(scope: Option<Scope>) -> Vec<DrawCmd> {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Context::new(skin).paint(
            &mut list,
            &mut text,
            &Viewed {
                breadcrumb: "All Tracks".to_owned(),
                scope,
            },
            BOUNDS,
        );
        list.finish().commands().to_vec()
    }

    fn words(commands: &[DrawCmd]) -> Vec<(String, f32)> {
        commands
            .iter()
            .filter_map(|command| match command {
                DrawCmd::Text {
                    content, transform, ..
                } => Some((content.as_str().to_owned(), transform.dx)),
                _ => None,
            })
            .collect()
    }

    /// Without a scope the strip is an icon and a path. The picked scope, the
    /// chevron between them, and the room they take belong to a document that
    /// declared more than one scope, so none of them may appear here.
    #[kithara::test]
    fn a_strip_without_a_scope_shows_only_the_path() {
        let words = words(&drawn(None));

        assert_eq!(
            words
                .iter()
                .map(|(word, _)| word.as_str())
                .collect::<Vec<_>>(),
            ["All Tracks"]
        );
    }

    /// With one, the picked word comes first, then the chevron, then the path —
    /// each clear of the last.
    #[kithara::test]
    fn a_scope_reads_left_to_right_into_the_path() {
        let words = words(&drawn(Some(Scope {
            items: vec!["ZVUK".to_owned(), "LOCAL".to_owned()],
            selected: Some(1),
        })));

        assert_eq!(
            words
                .iter()
                .map(|(word, _)| word.as_str())
                .collect::<Vec<_>>(),
            ["LOCAL", SEPARATOR, "All Tracks"]
        );
        assert!(words.windows(2).all(|pair| pair[0].1 < pair[1].1));
    }

    /// The hairline is the last thing drawn and sits under the content, not
    /// across it: the strip has to end where the panel below it starts.
    #[kithara::test]
    fn the_hairline_closes_the_strip_along_its_bottom_edge() {
        let commands = drawn(None);
        let skin = builtin::skin();

        assert!(matches!(
            commands.last(),
            Some(DrawCmd::Fill { geom: crate::draw::Geom::Rect(rect), .. })
                if rect.y + rect.h == BOUNDS.y + skin.tree.context_height
                    && rect.h == skin.tree.context_divider_width
        ));
    }
}
