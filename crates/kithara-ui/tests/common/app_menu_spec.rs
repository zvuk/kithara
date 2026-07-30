use std::collections::BTreeMap;

use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    ids::EndpointId,
    module::{GlyphStyle, IconName, TextAlign, TextStyle},
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    size::{
        Dim::{self, Fill, Fixed, Shrink},
        SizeSpec,
    },
    skin::ColorRole::{
        self, Accent, BgFooter, BgSelect, Danger, Line, LineInner, LinePop, Muted, TextDim,
    },
    source::{MemResolver, UiConfig},
};

/// Entry every App Menu test compiles.
pub(crate) const LAYOUT: &str = "app-menu.klayout.ron";

const MODULE: &str = "modules/app-menu.kmodule.ron";

const MOUNT: &str = r#"(schema: "kithara.layout", version: 1, id: "app-menu",
    root: Module(instance: "app-menu", source: "modules/app-menu.kmodule.ron"))"#;

/// The one copy of the document. Every consumer reaches it through
/// [`resolver`].
const APP_MENU: &str = include_str!("../../assets/modules/app-menu.kmodule.ron");

/// Every endpoint the document names. The module owns it so a test binary
/// reaches the App Menu without pulling in the player fixtures beside it.
#[derive(Default)]
pub(crate) struct MenuRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl MenuRegistry {
    fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for MenuRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

pub(crate) fn resolver() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(LAYOUT, MOUNT);
    resolver.insert(MODULE, APP_MENU);
    resolver
}

pub(crate) fn registry() -> MenuRegistry {
    let mut registry = MenuRegistry::default();
    read_flags(&mut registry);
    read_text(&mut registry);
    commands(&mut registry);
    registry
}

pub(crate) fn menu() -> CompiledUi {
    compile(
        LAYOUT,
        &resolver(),
        &registry(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
    .expect("the App Menu compiles against the endpoints it names")
}

fn read_flags(registry: &mut MenuRegistry) {
    for id in [
        "ui.menu.open",
        "ui.window.can_open",
        "ui.prefs.wave_follow",
        "ui.prefs.autogain",
        "ui.prefs.mono",
        "ui.set.recording",
        "ui.set.casting",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Bool),
        );
    }
    for id in ["ui.menu.group_open", "ui.menu.group_hidden"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Bool).with_scope("group"),
        );
    }
    for id in [
        "ui.window.hidden",
        "ui.window.active",
        "ui.window.close_hidden",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Bool).with_scope("window"),
        );
    }
    registry.insert(
        EndpointCategory::Model,
        "ui.module.on",
        EndpointDesc::new(ValueKind::Bool).with_scope("module"),
    );
    registry.insert(
        EndpointCategory::Model,
        "ui.layout.selected",
        EndpointDesc::new(ValueKind::Bool).with_scope("layout"),
    );
}

fn read_text(registry: &mut MenuRegistry) {
    for id in [
        "ui.window.count",
        "ui.modules.title",
        "ui.modules.count",
        "ui.layouts.active",
        "ui.set.record_hint",
        "ui.set.cast_hint",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Text),
        );
    }
    for id in ["ui.window.title", "ui.window.caption"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Text).with_scope("window"),
        );
    }
}

fn commands(registry: &mut MenuRegistry) {
    for id in [
        "ui.menu.toggle",
        "ui.menu.close",
        "ui.window.open",
        "ui.window.toggle_full_screen",
        "ui.prefs.toggle_wave_follow",
        "ui.prefs.toggle_autogain",
        "ui.prefs.toggle_mono",
        "ui.set.toggle_record",
        "ui.set.toggle_cast",
        "ui.library.add_folder",
        "ui.settings.open",
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "ui.menu.toggle_group",
        EndpointDesc::new(ValueKind::Trigger).with_scope("group"),
    );
    for id in [
        "ui.window.focus",
        "ui.window.cycle_display",
        "ui.window.close",
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger).with_scope("window"),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "ui.module.toggle",
        EndpointDesc::new(ValueKind::Trigger).with_scope("module"),
    );
    registry.insert(
        EndpointCategory::Command,
        "ui.layout.apply",
        EndpointDesc::new(ValueKind::Trigger).with_scope("layout"),
    );
}

/// Hairlines a box declares, in the four-sided shape the schema carries.
/// [`FrameSides`](kithara_ui::layout::FrameSides) is `#[non_exhaustive]`, so a
/// table outside the crate cannot name one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Edges {
    pub(crate) top: bool,
    pub(crate) right: bool,
    pub(crate) bottom: bool,
    pub(crate) left: bool,
}

impl Edges {
    pub(crate) const TOP: Self = Self::new(true, false, false, false);
    pub(crate) const RIGHT: Self = Self::new(false, true, false, false);
    pub(crate) const BOTTOM: Self = Self::new(false, false, true, false);
    pub(crate) const BAND: Self = Self::new(true, false, true, false);
    pub(crate) const ALL: Self = Self::new(true, true, true, true);

    const fn new(top: bool, right: bool, bottom: bool, left: bool) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }
}

/// One row of the plan's §2.4 geometry table. `gap` and the paddings are the
/// values the renderer resolves, so an omitted `gap` reads `1.0`, never `0.0`.
/// All three are `None` on a `Pressable` or `Popover` that wraps no container.
#[derive(Debug, PartialEq)]
pub(crate) struct Box<'a> {
    pub(crate) id: &'a str,
    pub(crate) size: Option<SizeSpec>,
    pub(crate) gap: Option<f32>,
    pub(crate) pad_x: Option<f32>,
    pub(crate) pad_y: Option<f32>,
    pub(crate) frame: Option<Edges>,
    pub(crate) frame_color: Option<ColorRole>,
    pub(crate) active_frame_color: Option<ColorRole>,
    pub(crate) background: Option<ColorRole>,
    pub(crate) active_background: Option<ColorRole>,
    pub(crate) press: Option<&'a str>,
    pub(crate) active: Option<&'a str>,
}

impl<'a> Box<'a> {
    const fn new(id: &'a str, w: Dim, h: Dim, gap: f32) -> Self {
        Self {
            id,
            size: Some(SizeSpec::new(w, h)),
            gap: Some(gap),
            pad_x: Some(0.0),
            pad_y: Some(0.0),
            frame: None,
            frame_color: None,
            active_frame_color: None,
            background: None,
            active_background: None,
            press: None,
            active: None,
        }
    }

    pub(crate) const fn bare(id: &'a str) -> Self {
        Self {
            id,
            size: None,
            gap: None,
            pad_x: None,
            pad_y: None,
            frame: None,
            frame_color: None,
            active_frame_color: None,
            background: None,
            active_background: None,
            press: None,
            active: None,
        }
    }

    const fn pad_x(mut self, pad_x: f32) -> Self {
        self.pad_x = Some(pad_x);
        self
    }

    const fn frame(mut self, edges: Edges, color: ColorRole) -> Self {
        self.frame = Some(edges);
        self.frame_color = Some(color);
        self
    }

    const fn active_frame(mut self, color: ColorRole) -> Self {
        self.active_frame_color = Some(color);
        self
    }

    const fn fill(mut self, color: ColorRole) -> Self {
        self.background = Some(color);
        self
    }

    const fn active_fill(mut self, color: ColorRole) -> Self {
        self.active_background = Some(color);
        self
    }

    pub(crate) const fn press(mut self, endpoint: &'a str) -> Self {
        self.press = Some(endpoint);
        self
    }

    pub(crate) const fn active(mut self, endpoint: &'a str) -> Self {
        self.active = Some(endpoint);
        self
    }
}

/// One row of the plan's §2.5 run table.
#[derive(Debug, PartialEq)]
pub(crate) struct Run<'a> {
    pub(crate) id: &'a str,
    pub(crate) style: TextStyle,
    pub(crate) align: TextAlign,
    pub(crate) size: Option<SizeSpec>,
    pub(crate) label: Option<&'a str>,
    pub(crate) read: Option<&'a str>,
    pub(crate) active: Option<&'a str>,
}

impl<'a> Run<'a> {
    const fn new(id: &'a str, style: TextStyle) -> Self {
        Self {
            id,
            style,
            align: TextAlign::Start,
            size: None,
            label: None,
            read: None,
            active: None,
        }
    }

    const fn size(mut self, w: Dim, h: Dim) -> Self {
        self.size = Some(SizeSpec::new(w, h));
        self
    }

    const fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    const fn read(mut self, endpoint: &'a str) -> Self {
        self.read = Some(endpoint);
        self
    }

    const fn active(mut self, endpoint: &'a str) -> Self {
        self.active = Some(endpoint);
        self
    }
}

/// One row of the plan's §2.5 glyph table.
#[derive(Debug, PartialEq)]
pub(crate) struct Mark<'a> {
    pub(crate) id: &'a str,
    pub(crate) icon: IconName,
    pub(crate) active_icon: Option<IconName>,
    pub(crate) style: GlyphStyle,
    pub(crate) color: Option<ColorRole>,
    pub(crate) active_color: Option<ColorRole>,
    pub(crate) active: Option<&'a str>,
}

impl<'a> Mark<'a> {
    const fn new(id: &'a str, icon: IconName, style: GlyphStyle, color: ColorRole) -> Self {
        Self {
            id,
            icon,
            active_icon: None,
            style,
            color: Some(color),
            active_color: None,
            active: None,
        }
    }

    const fn active_icon(mut self, icon: IconName) -> Self {
        self.active_icon = Some(icon);
        self
    }

    const fn active_color(mut self, color: ColorRole) -> Self {
        self.active_color = Some(color);
        self
    }

    const fn active(mut self, endpoint: &'a str) -> Self {
        self.active = Some(endpoint);
        self
    }
}

/// One `Optional`: the node it wraps, and the Bool that hides it.
#[derive(Debug, PartialEq)]
pub(crate) struct Block<'a> {
    pub(crate) id: &'a str,
    pub(crate) wraps: &'a str,
    pub(crate) hidden: &'a str,
}

/// §2.4, row by row, in document order.
pub(crate) const BOXES: [Box<'static>; 82] = [
    Box::new("menu", Shrink, Fill, 0.0),
    Box::bare("pop").active("ui.menu.open"),
    Box::new("burger", Fixed(36.0), Fill, 0.0)
        .frame(Edges::RIGHT, LineInner)
        .active_fill(BgSelect)
        .press("ui.menu.toggle")
        .active("ui.menu.open"),
    Box::new("surface", Fixed(298.0), Shrink, 0.0),
    Box::new("header", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .frame(Edges::BOTTOM, LinePop),
    Box::bare("header-close").press("ui.menu.close"),
    Box::new("windows-head", Fill, Fixed(22.0), 0.0)
        .pad_x(10.0)
        .frame(Edges::BOTTOM, LinePop),
    Box::new("window-1", Fill, Fixed(30.0), 8.0)
        .pad_x(10.0)
        .active_fill(BgSelect)
        .press("ui.window.focus@window=1")
        .active("ui.window.active@window=1"),
    Box::new("window-1-text", Fill, Shrink, 1.0),
    Box::new("window-1-display", Fixed(18.0), Fixed(18.0), 0.0)
        .press("ui.window.cycle_display@window=1"),
    Box::new("window-1-close-cell", Fixed(18.0), Fixed(18.0), 0.0),
    Box::bare("window-1-close").press("ui.window.close@window=1"),
    Box::new("window-2", Fill, Fixed(30.0), 8.0)
        .pad_x(10.0)
        .active_fill(BgSelect)
        .press("ui.window.focus@window=2")
        .active("ui.window.active@window=2"),
    Box::new("window-2-text", Fill, Shrink, 1.0),
    Box::new("window-2-display", Fixed(18.0), Fixed(18.0), 0.0)
        .press("ui.window.cycle_display@window=2"),
    Box::new("window-2-close-cell", Fixed(18.0), Fixed(18.0), 0.0),
    Box::bare("window-2-close").press("ui.window.close@window=2"),
    Box::new("window-3", Fill, Fixed(30.0), 8.0)
        .pad_x(10.0)
        .active_fill(BgSelect)
        .press("ui.window.focus@window=3")
        .active("ui.window.active@window=3"),
    Box::new("window-3-text", Fill, Shrink, 1.0),
    Box::new("window-3-display", Fixed(18.0), Fixed(18.0), 0.0)
        .press("ui.window.cycle_display@window=3"),
    Box::new("window-3-close-cell", Fixed(18.0), Fixed(18.0), 0.0),
    Box::bare("window-3-close").press("ui.window.close@window=3"),
    Box::new("new-window", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .frame(Edges::TOP, LinePop)
        .press("ui.window.open"),
    Box::new("modules-head", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .frame(Edges::TOP, LinePop)
        .press("ui.menu.toggle_group@group=mod"),
    Box::new("modules-grid", Fill, Shrink, 0.0).pad_x(10.0),
    Box::new("modules-indent", Fixed(17.0), Fill, 0.0),
    Box::new("modules-column", Fill, Shrink, 4.0),
    Box::new("modules-row-1", Fill, Fixed(20.0), 4.0),
    Box::new("module-ov", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=ov")
        .active("ui.module.on@module=ov"),
    Box::new("module-ov-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=ov"),
    Box::new("module-mix", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=mix")
        .active("ui.module.on@module=mix"),
    Box::new("module-mix-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=mix"),
    Box::new("module-fx1", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=fx1")
        .active("ui.module.on@module=fx1"),
    Box::new("module-fx1-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=fx1"),
    Box::new("modules-row-2", Fill, Fixed(20.0), 4.0),
    Box::new("module-fx2", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=fx2")
        .active("ui.module.on@module=fx2"),
    Box::new("module-fx2-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=fx2"),
    Box::new("module-vcf", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=vcf")
        .active("ui.module.on@module=vcf"),
    Box::new("module-vcf-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=vcf"),
    Box::new("module-rec", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=rec")
        .active("ui.module.on@module=rec"),
    Box::new("module-rec-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=rec"),
    Box::new("modules-row-3", Fill, Fixed(20.0), 4.0),
    Box::new("module-vis", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=vis")
        .active("ui.module.on@module=vis"),
    Box::new("module-vis-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=vis"),
    Box::new("module-tl", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=tl")
        .active("ui.module.on@module=tl"),
    Box::new("module-tl-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=tl"),
    Box::new("module-cpu", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=cpu")
        .active("ui.module.on@module=cpu"),
    Box::new("module-cpu-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=cpu"),
    Box::new("modules-row-4", Fill, Fixed(20.0), 4.0),
    Box::new("module-net", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=net")
        .active("ui.module.on@module=net"),
    Box::new("module-net-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=net"),
    Box::new("module-buf", Fill, Fixed(20.0), 5.0)
        .pad_x(5.0)
        .frame(Edges::ALL, Line)
        .active_frame(Accent)
        .active_fill(BgSelect)
        .press("ui.module.toggle@module=buf")
        .active("ui.module.on@module=buf"),
    Box::new("module-buf-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.module.on@module=buf"),
    Box::new("modules-filler", Fill, Fixed(20.0), 0.0),
    Box::new("modules-space", Fill, Fixed(8.0), 0.0),
    Box::new("layouts-head", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .frame(Edges::TOP, LinePop)
        .press("ui.menu.toggle_group@group=lay"),
    Box::new("layouts-list", Fill, Shrink, 0.0),
    Box::new("layout-1", Fill, Fixed(24.0), 8.0)
        .pad_x(10.0)
        .press("ui.layout.apply@layout=1"),
    Box::new("layout-1-indent", Fixed(9.0), Fill, 0.0),
    Box::new("layout-1-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.layout.selected@layout=1"),
    Box::new("layout-2", Fill, Fixed(24.0), 8.0)
        .pad_x(10.0)
        .press("ui.layout.apply@layout=2"),
    Box::new("layout-2-indent", Fixed(9.0), Fill, 0.0),
    Box::new("layout-2-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.layout.selected@layout=2"),
    Box::new("layout-3", Fill, Fixed(24.0), 8.0)
        .pad_x(10.0)
        .press("ui.layout.apply@layout=3"),
    Box::new("layout-3-indent", Fixed(9.0), Fill, 0.0),
    Box::new("layout-3-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.layout.selected@layout=3"),
    Box::new("layout-4", Fill, Fixed(24.0), 8.0)
        .pad_x(10.0)
        .press("ui.layout.apply@layout=4"),
    Box::new("layout-4-indent", Fixed(9.0), Fill, 0.0),
    Box::new("layout-4-led", Fixed(4.0), Fixed(4.0), 0.0)
        .fill(Line)
        .active_fill(Accent)
        .active("ui.layout.selected@layout=4"),
    Box::new("save", Fill, Fixed(24.0), 8.0).pad_x(10.0),
    Box::new("save-indent", Fixed(9.0), Fill, 0.0),
    Box::new("layouts-space", Fill, Fixed(6.0), 0.0),
    Box::new("full-screen", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .frame(Edges::TOP, LinePop)
        .press("ui.window.toggle_full_screen"),
    Box::new("wave-follow", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .press("ui.prefs.toggle_wave_follow"),
    Box::new("mixing-label", Fill, Fixed(22.0), 0.0)
        .pad_x(10.0)
        .fill(BgFooter)
        .frame(Edges::BAND, LineInner),
    Box::new("autogain", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .press("ui.prefs.toggle_autogain"),
    Box::new("mono", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .press("ui.prefs.toggle_mono"),
    Box::new("set-label", Fill, Fixed(22.0), 0.0)
        .pad_x(10.0)
        .fill(BgFooter)
        .frame(Edges::BAND, LineInner),
    Box::new("record", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .press("ui.set.toggle_record"),
    Box::new("cast", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .press("ui.set.toggle_cast"),
    Box::new("add-folder", Fill, Fixed(26.0), 8.0)
        .pad_x(10.0)
        .press("ui.library.add_folder"),
    Box::new("settings", Fill, Fixed(30.0), 8.0)
        .pad_x(10.0)
        .fill(BgFooter)
        .frame(Edges::TOP, LinePop)
        .press("ui.settings.open"),
];

/// §2.5 runs, in document order.
pub(crate) const RUNS: [Run<'static>; 46] = [
    Run::new("brand", TextStyle::BrandSmall)
        .size(Fill, Fill)
        .label("KITHARA"),
    Run::new("version", TextStyle::MenuCount).label("2.4.1"),
    Run::new("windows-head-label", TextStyle::MenuSection)
        .size(Fill, Fill)
        .label("ОКНА И МОДУЛИ"),
    Run::new("windows-head-count", TextStyle::MenuCount).read("ui.window.count"),
    Run::new("window-1-title", TextStyle::MenuRow)
        .size(Fill, Fixed(13.0))
        .read("ui.window.title@window=1")
        .active("ui.window.active@window=1"),
    Run::new("window-1-caption", TextStyle::MenuCaption)
        .size(Fill, Fixed(9.0))
        .read("ui.window.caption@window=1"),
    Run::new("window-2-title", TextStyle::MenuRow)
        .size(Fill, Fixed(13.0))
        .read("ui.window.title@window=2")
        .active("ui.window.active@window=2"),
    Run::new("window-2-caption", TextStyle::MenuCaption)
        .size(Fill, Fixed(9.0))
        .read("ui.window.caption@window=2"),
    Run::new("window-3-title", TextStyle::MenuRow)
        .size(Fill, Fixed(13.0))
        .read("ui.window.title@window=3")
        .active("ui.window.active@window=3"),
    Run::new("window-3-caption", TextStyle::MenuCaption)
        .size(Fill, Fixed(9.0))
        .read("ui.window.caption@window=3"),
    Run::new("new-window-label", TextStyle::MenuRowAccent)
        .size(Fill, Fill)
        .label("Новое окно")
        .active("ui.window.can_open"),
    Run::new("modules-title", TextStyle::MenuRowStrong)
        .size(Fill, Fill)
        .read("ui.modules.title"),
    Run::new("modules-count", TextStyle::MenuHint).read("ui.modules.count"),
    Run::new("module-ov-label", TextStyle::MenuCell)
        .label("OVERVIEW")
        .active("ui.module.on@module=ov"),
    Run::new("module-mix-label", TextStyle::MenuCell)
        .label("MIXER")
        .active("ui.module.on@module=mix"),
    Run::new("module-fx1-label", TextStyle::MenuCell)
        .label("FX 1")
        .active("ui.module.on@module=fx1"),
    Run::new("module-fx2-label", TextStyle::MenuCell)
        .label("FX 2")
        .active("ui.module.on@module=fx2"),
    Run::new("module-vcf-label", TextStyle::MenuCell)
        .label("VCF")
        .active("ui.module.on@module=vcf"),
    Run::new("module-rec-label", TextStyle::MenuCell)
        .label("REC")
        .active("ui.module.on@module=rec"),
    Run::new("module-vis-label", TextStyle::MenuCell)
        .label("VISUAL")
        .active("ui.module.on@module=vis"),
    Run::new("module-tl-label", TextStyle::MenuCell)
        .label("TIMELINE")
        .active("ui.module.on@module=tl"),
    Run::new("module-cpu-label", TextStyle::MenuCell)
        .label("CPU")
        .active("ui.module.on@module=cpu"),
    Run::new("module-net-label", TextStyle::MenuCell)
        .label("NET")
        .active("ui.module.on@module=net"),
    Run::new("module-buf-label", TextStyle::MenuCell)
        .label("BUF")
        .active("ui.module.on@module=buf"),
    Run::new("layouts-title", TextStyle::MenuRowStrong)
        .size(Fill, Fill)
        .label("Сохранённые раскладки"),
    Run::new("layouts-active", TextStyle::MenuHintAccent).read("ui.layouts.active"),
    Run::new("layout-1-label", TextStyle::MenuList)
        .size(Fill, Fill)
        .label("КЛУБ · 2 ДЕКИ")
        .active("ui.layout.selected@layout=1"),
    Run::new("layout-2-label", TextStyle::MenuList)
        .size(Fill, Fill)
        .label("СТУДИЯ · 4 ДЕКИ + VST")
        .active("ui.layout.selected@layout=2"),
    Run::new("layout-3-label", TextStyle::MenuList)
        .size(Fill, Fill)
        .label("ВИЗУАЛ + ТАЙМЛАЙН")
        .active("ui.layout.selected@layout=3"),
    Run::new("layout-4-label", TextStyle::MenuList)
        .size(Fill, Fill)
        .label("УЗКОЕ ОКНО · ТАБЫ")
        .active("ui.layout.selected@layout=4"),
    Run::new("save-label", TextStyle::MenuList)
        .size(Fill, Fill)
        .label("Сохранить раскладку окна…"),
    Run::new("full-screen-label", TextStyle::MenuRow)
        .size(Fill, Fill)
        .label("Полный экран · активное окно"),
    Run::new("full-screen-hint", TextStyle::MenuHint).label("⌃⌘F"),
    Run::new("wave-follow-label", TextStyle::MenuRow)
        .size(Fill, Fill)
        .label("Волна следует за плейхедом"),
    Run::new("mixing-label-text", TextStyle::MenuSection)
        .size(Fill, Fill)
        .label("СВЕДЕНИЕ"),
    Run::new("autogain-label", TextStyle::MenuRow)
        .size(Fill, Fill)
        .label("Автогейн по треку"),
    Run::new("mono-label", TextStyle::MenuRow)
        .size(Fill, Fill)
        .label("Моно-выход"),
    Run::new("set-label-text", TextStyle::MenuSection)
        .size(Fill, Fill)
        .label("СЕТ"),
    Run::new("record-label", TextStyle::MenuRow)
        .size(Fill, Fill)
        .label("Запись сета"),
    Run::new("record-hint", TextStyle::MenuHint).read("ui.set.record_hint"),
    Run::new("cast-label", TextStyle::MenuRow)
        .size(Fill, Fill)
        .label("Трансляция"),
    Run::new("cast-hint", TextStyle::MenuHint).read("ui.set.cast_hint"),
    Run::new("add-folder-label", TextStyle::MenuRow)
        .size(Fill, Fill)
        .label("Добавить папку…"),
    Run::new("add-folder-hint", TextStyle::MenuHint).label("⌘O"),
    Run::new("settings-label", TextStyle::MenuRowStrong)
        .size(Fill, Fill)
        .label("Настройки…"),
    Run::new("settings-hint", TextStyle::MenuHint).label("⌘,"),
];

/// §2.5 glyphs, in document order.
pub(crate) const MARKS: [Mark<'static>; 23] = [
    Mark::new(
        "burger-icon",
        IconName::Menu,
        GlyphStyle::MenuBurger,
        TextDim,
    ),
    Mark::new(
        "header-close-icon",
        IconName::X,
        GlyphStyle::MenuSmall,
        Muted,
    ),
    Mark::new("window-1-icon", IconName::Monitor, GlyphStyle::Menu, Muted)
        .active_color(Accent)
        .active("ui.window.active@window=1"),
    Mark::new(
        "window-1-display-icon",
        IconName::RefreshCw,
        GlyphStyle::MenuCell,
        Muted,
    ),
    Mark::new(
        "window-1-close-icon",
        IconName::X,
        GlyphStyle::MenuCell,
        Muted,
    ),
    Mark::new("window-2-icon", IconName::Monitor, GlyphStyle::Menu, Muted)
        .active_color(Accent)
        .active("ui.window.active@window=2"),
    Mark::new(
        "window-2-display-icon",
        IconName::RefreshCw,
        GlyphStyle::MenuCell,
        Muted,
    ),
    Mark::new(
        "window-2-close-icon",
        IconName::X,
        GlyphStyle::MenuCell,
        Muted,
    ),
    Mark::new("window-3-icon", IconName::Monitor, GlyphStyle::Menu, Muted)
        .active_color(Accent)
        .active("ui.window.active@window=3"),
    Mark::new(
        "window-3-display-icon",
        IconName::RefreshCw,
        GlyphStyle::MenuCell,
        Muted,
    ),
    Mark::new(
        "window-3-close-icon",
        IconName::X,
        GlyphStyle::MenuCell,
        Muted,
    ),
    Mark::new("new-window-icon", IconName::Plus, GlyphStyle::Menu, Muted)
        .active_color(Accent)
        .active("ui.window.can_open"),
    Mark::new(
        "modules-caret",
        IconName::ChevronRight,
        GlyphStyle::Menu,
        Muted,
    )
    .active_icon(IconName::ChevronDown)
    .active("ui.menu.group_open@group=mod"),
    Mark::new(
        "layouts-caret",
        IconName::ChevronRight,
        GlyphStyle::Menu,
        Muted,
    )
    .active_icon(IconName::ChevronDown)
    .active("ui.menu.group_open@group=lay"),
    Mark::new("save-icon", IconName::Save, GlyphStyle::MenuSmall, Muted),
    Mark::new(
        "full-screen-icon",
        IconName::Maximize,
        GlyphStyle::Menu,
        Muted,
    ),
    Mark::new(
        "wave-follow-icon",
        IconName::Activity,
        GlyphStyle::Menu,
        Line,
    )
    .active_color(Accent)
    .active("ui.prefs.wave_follow"),
    Mark::new(
        "autogain-icon",
        IconName::SlidersHorizontal,
        GlyphStyle::Menu,
        Line,
    )
    .active_color(Accent)
    .active("ui.prefs.autogain"),
    Mark::new("mono-icon", IconName::Circle, GlyphStyle::Menu, Line)
        .active_color(Accent)
        .active("ui.prefs.mono"),
    Mark::new("record-icon", IconName::Disc, GlyphStyle::Menu, Line)
        .active_color(Danger)
        .active("ui.set.recording"),
    Mark::new("cast-icon", IconName::Radio, GlyphStyle::Menu, Line)
        .active_color(Accent)
        .active("ui.set.casting"),
    Mark::new(
        "add-folder-icon",
        IconName::FolderPlus,
        GlyphStyle::Menu,
        Muted,
    ),
    Mark::new("settings-icon", IconName::Gear, GlyphStyle::Menu, Muted),
];

/// Every `Optional`, in document order.
pub(crate) const BLOCKS: [Block<'static>; 8] = [
    Block {
        id: "window-1-close-block",
        wraps: "window-1-close",
        hidden: "ui.window.close_hidden@window=1",
    },
    Block {
        id: "window-2-block",
        wraps: "window-2",
        hidden: "ui.window.hidden@window=2",
    },
    Block {
        id: "window-2-close-block",
        wraps: "window-2-close",
        hidden: "ui.window.close_hidden@window=2",
    },
    Block {
        id: "window-3-block",
        wraps: "window-3",
        hidden: "ui.window.hidden@window=3",
    },
    Block {
        id: "window-3-close-block",
        wraps: "window-3-close",
        hidden: "ui.window.close_hidden@window=3",
    },
    Block {
        id: "modules-block",
        wraps: "modules-grid",
        hidden: "ui.menu.group_hidden@group=mod",
    },
    Block {
        id: "modules-space-block",
        wraps: "modules-space",
        hidden: "ui.menu.group_hidden@group=mod",
    },
    Block {
        id: "layouts-block",
        wraps: "layouts-list",
        hidden: "ui.menu.group_hidden@group=lay",
    },
];
