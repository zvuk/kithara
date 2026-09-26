use std::{borrow::Cow, env, fmt::Write as _, fs, path::Path};

use iced::{
    Pixels, Rectangle, Size,
    advanced::{
        graphics::text::font_system,
        layout::{Layout, Limits},
        widget::Tree,
    },
};
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_tiny_skia::Renderer as TinySkiaRenderer;
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi, compile},
    expand::ExpandedNode,
    geom::Pt,
    ids::SourceUri,
    module::ChromeStyle,
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{
        Clock, ReadValue, Reads, Skin, StereoLevels, TableCell, TableRow, WaveBucket, WaveformView,
        fonts::{FONT_BYTES, SANS},
        tree,
    },
    shaping::{FontPolicy, GlyphFace, GlyphSegment, TextContext},
    source::{MemResolver, UiConfig},
    view,
};

struct FixtureReads {
    /// Where the application says a carried placement stands, for the fixtures
    /// that have one; nothing answers for the rest.
    placed: Option<Pt>,
    tracks: [TableRow<'static>; 3],
    buckets: [WaveBucket; 6],
    beats: [f32; 4],
    cues: [f32; 2],
    downbeats: [f32; 2],
}

impl Default for FixtureReads {
    fn default() -> Self {
        Self {
            beats: [0.125, 0.375, 0.625, 0.875],
            placed: None,
            buckets: [
                WaveBucket {
                    low: 0.25,
                    mid: 0.50,
                    high: 0.75,
                },
                WaveBucket {
                    low: 0.65,
                    mid: 0.35,
                    high: 0.15,
                },
                WaveBucket {
                    low: 0.45,
                    mid: 0.80,
                    high: 0.30,
                },
                WaveBucket {
                    low: 0.90,
                    mid: 0.55,
                    high: 0.20,
                },
                WaveBucket {
                    low: 0.30,
                    mid: 0.60,
                    high: 0.85,
                },
                WaveBucket {
                    low: 0.70,
                    mid: 0.40,
                    high: 0.50,
                },
            ],
            cues: [0.25, 0.75],
            downbeats: [0.0, 0.5],
            tracks: [
                table_row((
                    "Midnight Circuit",
                    "Neon Lines",
                    "04:12",
                    Some("A"),
                    "128.0",
                    "8A",
                    82,
                    "Blend",
                    true,
                )),
                table_row((
                    "Signal Path",
                    "Static Motion",
                    "03:47",
                    None,
                    "124.5",
                    "10B",
                    68,
                    "Cut",
                    false,
                )),
                table_row((
                    "Afterimage",
                    "Glass Avenue",
                    "05:03",
                    Some("B"),
                    "126.0",
                    "7A",
                    74,
                    "Echo",
                    false,
                )),
            ],
        }
    }
}

type RowFixture = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
    &'static str,
    u8,
    &'static str,
    bool,
);

fn table_row(
    (title, artist, time, deck, bpm, key, energy, transition, selected): RowFixture,
) -> TableRow<'static> {
    let mut cells = vec![
        TableCell::text("title", title),
        TableCell::text("artist", artist),
        TableCell::text("time", time),
        TableCell::text("bpm", bpm),
        TableCell::text("key", key),
        TableCell::number("energy", energy),
        TableCell::text("transition", transition),
    ];
    if let Some(deck) = deck {
        cells.push(TableCell::text("deck", deck));
    }
    TableRow::new(cells, selected)
}

impl Reads for FixtureReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        match id {
            "scene.at" => self.placed.map(ReadValue::Point),
            "deck.playback.tempo" => Some(ReadValue::Text("128.0")),
            "deck.track.title" => Some(ReadValue::Text("Midnight Circuit")),
            "deck.playback.playing" | "deck.playback.looping" | "deck.playback.synced" => {
                Some(ReadValue::Bool(true))
            }
            "deck.playback.reverse" => Some(ReadValue::Bool(false)),
            "deck.playback.position_normalized" => Some(ReadValue::Scalar(0.375)),
            "deck.view.zoom" => Some(ReadValue::Scalar(0.25)),
            "player.output.volume" => Some(ReadValue::Scalar(0.8)),
            "player.output.levels" => Some(ReadValue::Stereo(StereoLevels {
                l: 0.64,
                r: 0.48,
                volume: 0.8,
            })),
            "deck.playback.waveform" => Some(ReadValue::Waveform(WaveformView {
                buckets: &self.buckets,
                revision: 0,
                beats: &self.beats,
                downbeats: &self.downbeats,
                unready: &[],
                bpm: Some(128.0),
                r#loop: Some([0.25, 0.5]),
                cues: &self.cues,
            })),
            "library.visible_tracks" => Some(ReadValue::Table(&self.tracks)),
            _ => None,
        }
    }
}

fn headless_renderer() -> iced::Renderer {
    let mut fonts = font_system()
        .write()
        .expect("iced font system lock must not be poisoned");
    for bytes in FONT_BYTES {
        fonts.load_font(Cow::Borrowed(bytes));
    }
    drop(fonts);

    FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)))
}

fn fixture_skin() -> Skin {
    Skin::resolve_with_font_policy(
        builtin::skin_doc().clone(),
        builtin::text_doc(),
        &SourceUri("fixture:layout-parity".to_owned()),
        &builtin::resolver(),
        FontPolicy::Embedded,
    )
    .expect("builtin layout fixture skin must resolve")
}

fn fixture_text_corpus() -> impl Iterator<Item = &'static str> {
    [
        "#",
        "1",
        "2",
        "3 TRACKS",
        "A",
        "Afterimage",
        "ART",
        "ARTIST",
        "B",
        "Blend",
        "BPM",
        "CUE",
        "Cut",
        "DECK",
        "Echo",
        "ENERGY",
        "Glass Avenue",
        "HLS AUTO/320 BUF 18S",
        "H",
        "I",
        "KEY",
        "K",
        "LOOP 4",
        "MICRO",
        "Midnight Circuit",
        "Neon Lines",
        "no source",
        "PAUSE",
        "PLAYER",
        "PLAY",
        "REMAIN",
        "Signal Path",
        "Static Motion",
        "SYNC",
        "TEMPO",
        "T",
        "TIME",
        "TITLE",
        "TRACKS",
        "TRANSITION",
        "R",
        "03:47",
        "04:12",
        "05:03",
        "7A",
        "8A",
        "10B",
        "124.5",
        "126.0",
        "128.0",
        "128.00",
        "—",
    ]
    .into_iter()
}

fn dump(
    preset: &str,
    ui: &CompiledUi,
    reads: &FixtureReads,
    skin: &Skin,
    renderer: &iced::Renderer,
    viewport: Size,
) -> String {
    let mut element = tree::render(
        &ui.root,
        ui,
        reads,
        &view::EMPTY,
        skin,
        Clock::default(),
        None,
    );
    let mut tree = Tree::new(element.as_widget());
    let node =
        element
            .as_widget_mut()
            .layout(&mut tree, renderer, &Limits::new(Size::ZERO, viewport));
    let mut output = String::new();
    writeln!(
        &mut output,
        "# {preset} @ {:.0}x{:.0}",
        viewport.width, viewport.height
    )
    .expect("writing to a String cannot fail");
    write_layout(&mut output, ui, reads, Layout::new(&node));
    output
}

#[derive(Default)]
struct Attribution {
    document_nodes: usize,
    document_rects: usize,
    furniture: usize,
    opaque_control_nodes: usize,
    wrappers: usize,
}

impl Attribution {
    const fn total(&self) -> usize {
        self.document_rects + self.wrappers + self.opaque_control_nodes + self.furniture
    }
}

struct LayoutWalker<'a> {
    ui: &'a CompiledUi,
    output: &'a mut String,
    reads: &'a dyn Reads,
    attribution: Attribution,
}

impl LayoutWalker<'_> {
    fn compiled(
        &mut self,
        node: &CompiledNode,
        layout: Layout<'_>,
        parent: &str,
        position: usize,
        depth: usize,
    ) {
        let path = compiled_path(node, self.ui, parent, position);
        match node {
            CompiledNode::Split { children, .. } => {
                self.document(&path, layout, depth, false);
                let flex = only_child(layout, &path, "split container");
                self.wrapper();
                let child_layouts = exact_children(flex, children.len(), &path, "split Flex");
                for (position, (cell, child_layout)) in
                    children.iter().zip(child_layouts).enumerate()
                {
                    self.compiled(&cell.node, child_layout, &path, position, depth + 1);
                }
            }
            CompiledNode::Module {
                instance,
                chrome,
                drop,
                collapsed,
                root,
                ..
            } => {
                self.document(&path, layout, depth, false);
                let shell_attributed = drop.is_none();
                let shell = if shell_attributed {
                    layout
                } else {
                    only_child(layout, &path, "module drop outline container")
                };
                let is_collapsed = *chrome == ChromeStyle::Full
                    && matches!(
                        self.reads.get(self.ui.resolve(*collapsed)),
                        Some(ReadValue::Bool(true))
                    );
                match chrome {
                    ChromeStyle::Frame => {
                        self.framed_module(root, shell, shell_attributed, &path, depth);
                    }
                    ChromeStyle::Plain => {
                        self.expanded(root, shell, &path, 0, depth + 1, shell_attributed);
                    }
                    ChromeStyle::Full => {
                        self.full_module(root, shell, shell_attributed, is_collapsed, &path, depth);
                    }
                    _ => panic!("unsupported chrome style at document path `{path}`"),
                }
                assert_eq!(
                    path,
                    self.ui.resolve(*instance),
                    "compiled module path changed while walking `{path}`"
                );
            }
            _ => panic!("unsupported compiled node at document path `{path}`"),
        }
    }

    fn document(&mut self, path: &str, layout: Layout<'_>, depth: usize, already_attributed: bool) {
        let bounds = layout.bounds();
        assert!(
            [bounds.x, bounds.y, bounds.width, bounds.height]
                .iter()
                .all(|value| value.is_finite()),
            "layout for document path `{path}` contains non-finite bounds: {bounds:?}"
        );
        for _ in 0..depth {
            self.output.push_str("  ");
        }
        writeln!(
            self.output,
            "{path} {:.3} {:.3} {:.3} {:.3}",
            bounds.x, bounds.y, bounds.width, bounds.height
        )
        .expect("writing to a String cannot fail");
        self.attribution.document_nodes += 1;
        if !already_attributed {
            self.attribution.document_rects += 1;
        }
    }

    fn expanded(
        &mut self,
        node: &ExpandedNode,
        layout: Layout<'_>,
        parent: &str,
        position: usize,
        depth: usize,
        already_attributed: bool,
    ) {
        let path = expanded_path(node, self.ui, parent, position);
        match node {
            ExpandedNode::Row {
                size,
                frame,
                surface,
                children,
                ..
            } => self.group(
                layout,
                &path,
                depth,
                already_attributed,
                &Group {
                    children,
                    flex_name: "Row",
                    framed: frame.is_some(),
                    sized: size.is_some(),
                    surfaced: surface.is_some(),
                },
            ),
            ExpandedNode::Column {
                size,
                frame,
                surface,
                children,
                ..
            } => self.group(
                layout,
                &path,
                depth,
                already_attributed,
                &Group {
                    children,
                    flex_name: "Column",
                    framed: frame.is_some(),
                    sized: size.is_some(),
                    surfaced: surface.is_some(),
                },
            ),
            ExpandedNode::Slot { size, children, .. } => {
                self.document(&path, layout, depth, already_attributed);
                let mut content = layout;
                if size.is_some() {
                    content = only_child(content, &path, "slot apply_size container");
                    self.wrapper();
                }
                let flex = only_child(content, &path, "slot container");
                self.wrapper();
                let child_layouts = exact_children(flex, children.len(), &path, "slot Flex");
                for (position, (child, child_layout)) in
                    children.iter().zip(child_layouts).enumerate()
                {
                    self.expanded(child, child_layout, &path, position, depth + 1, false);
                }
            }
            ExpandedNode::Control { .. }
            | ExpandedNode::Popover { .. }
            | ExpandedNode::Pressable { .. } => {
                self.document(&path, layout, depth, already_attributed);
                self.opaque_control_children(layout);
            }
            ExpandedNode::Optional { child, .. } => {
                self.expanded(child, layout, parent, position, depth, already_attributed);
            }
            // A revealed cell takes the place of its child when the room
            // reaches its band, and keeps the empty node `Flex` left for it
            // when it does not. Both hosts mount the child either way, so the
            // walk names it either way and only the box goes missing.
            ExpandedNode::Reveal { child, .. } => {
                if stood(layout) {
                    self.expanded(child, layout, parent, position, depth, already_attributed);
                } else {
                    self.unreached(child, parent, position, depth);
                    self.furniture(layout);
                }
            }
            _ => panic!("unsupported expanded node at document path `{path}`"),
        }
    }

    fn framed_module(
        &mut self,
        root: &ExpandedNode,
        shell: Layout<'_>,
        shell_attributed: bool,
        path: &str,
        depth: usize,
    ) {
        if !shell_attributed {
            self.wrapper();
        }
        let shell_children = exact_children(shell, 2, path, "module frame Stack");
        let body = shell_children[0];
        self.wrapper();
        self.furniture(shell_children[1]);
        let root_layout = only_child(body, path, "module frame body container");
        self.expanded(root, root_layout, path, 0, depth + 1, false);
    }

    fn full_module(
        &mut self,
        root: &ExpandedNode,
        shell: Layout<'_>,
        shell_attributed: bool,
        collapsed: bool,
        path: &str,
        depth: usize,
    ) {
        if !shell_attributed {
            self.wrapper();
        }
        let shell_children = exact_children(shell, 2, path, "full module frame Stack");
        let body = shell_children[0];
        self.wrapper();
        self.furniture(shell_children[1]);
        let content = only_child(body, path, "full module frame body container");
        if collapsed {
            self.furniture(content);
            return;
        }

        self.wrapper();
        let chrome = exact_children(content, 5, path, "full module chrome Column");
        for (index, furniture) in chrome.iter().copied().enumerate() {
            if index != 2 {
                self.furniture(furniture);
            }
        }
        self.wrapper();
        let root_layout = only_child(chrome[2], path, "full module content container");
        self.expanded(root, root_layout, path, 0, depth + 1, false);
    }

    fn furniture(&mut self, layout: Layout<'_>) {
        self.attribution.furniture += layout_node_count(layout);
    }

    fn group(
        &mut self,
        layout: Layout<'_>,
        path: &str,
        depth: usize,
        already_attributed: bool,
        group: &Group<'_>,
    ) {
        self.document(path, layout, depth, already_attributed);
        let mut content = layout;
        if group.sized {
            content = only_child(content, path, "apply_size container");
            self.wrapper();
        }
        if group.surfaced {
            let surface_children = exact_children(content, 2, path, "wheel surface Stack");
            content = surface_children[0];
            self.wrapper();
            self.furniture(surface_children[1]);
        }
        if group.framed {
            let frame_children = exact_children(content, 2, path, "frame overlay Stack");
            content = frame_children[0];
            self.wrapper();
            self.furniture(frame_children[1]);
            content = only_child(content, path, "frame overlay body container");
            self.wrapper();
        }
        let flex = only_child(content, path, "padding container");
        self.wrapper();
        let child_layouts = exact_children(flex, group.children.len(), path, group.flex_name);
        for (position, (child, child_layout)) in
            group.children.iter().zip(child_layouts).enumerate()
        {
            self.expanded(child, child_layout, path, position, depth + 1, false);
        }
    }

    fn opaque_control_children(&mut self, layout: Layout<'_>) {
        self.attribution.opaque_control_nodes +=
            layout.children().map(layout_node_count).sum::<usize>();
    }

    /// Writes a document node the room never reached, which carries a path and
    /// no box.
    fn unplaced(&mut self, path: &str, depth: usize) {
        for _ in 0..depth {
            self.output.push_str("  ");
        }
        writeln!(self.output, "{path} -").expect("writing to a String cannot fail");
        self.attribution.document_nodes += 1;
    }

    /// Names every document node beneath a cell the room never reached.
    ///
    /// Nothing under such a cell is laid out, so there is no iced node to walk
    /// and the paths come from the document alone.
    fn unreached(&mut self, node: &ExpandedNode, parent: &str, position: usize, depth: usize) {
        let path = expanded_path(node, self.ui, parent, position);
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                self.unplaced(&path, depth);
                for (position, child) in children.iter().enumerate() {
                    self.unreached(child, &path, position, depth + 1);
                }
            }
            ExpandedNode::Optional { child, .. } | ExpandedNode::Reveal { child, .. } => {
                self.unreached(child, parent, position, depth);
            }
            ExpandedNode::Control { .. }
            | ExpandedNode::Popover { .. }
            | ExpandedNode::Pressable { .. } => self.unplaced(&path, depth),
            _ => panic!("unsupported expanded node at document path `{path}`"),
        }
    }

    fn wrapper(&mut self) {
        self.attribution.wrappers += 1;
    }
}

/// The wrapper layers a Row or Column asks iced for, and the children beneath
/// them. They travel together because every one of them is read off the same
/// expanded node.
struct Group<'a> {
    children: &'a [ExpandedNode],
    flex_name: &'static str,
    framed: bool,
    sized: bool,
    surfaced: bool,
}

fn write_layout(output: &mut String, ui: &CompiledUi, reads: &dyn Reads, layout: Layout<'_>) {
    let root_path = compiled_path(&ui.root, ui, "", 0);
    let total = layout_node_count(layout);
    let mut walker = LayoutWalker {
        output,
        ui,
        reads,
        attribution: Attribution::default(),
    };
    let content = if ui.resize_edges || ui.dragged.is_some() {
        walker.wrapper();
        exact_children(layout, 1, &root_path, "window host layer")[0]
    } else {
        layout
    };
    walker.compiled(&ui.root, content, "", 0, 0);
    assert_eq!(
        walker.attribution.document_nodes,
        document_node_count(&ui.root, ui, reads),
        "the walk emitted {} document nodes but the document has {}; a subtree was skipped",
        walker.attribution.document_nodes,
        document_node_count(&ui.root, ui, reads),
    );
    assert_eq!(
        walker.attribution.total(),
        total,
        "layout attribution mismatch at document path `{root_path}`: attributed {} of {total} \
         iced nodes ({} document nodes emitted, {} document rects, {} wrappers, {} opaque \
         control nodes, {} furniture nodes)",
        walker.attribution.total(),
        walker.attribution.document_nodes,
        walker.attribution.document_rects,
        walker.attribution.wrappers,
        walker.attribution.opaque_control_nodes,
        walker.attribution.furniture,
    );
}

/// Counts document nodes without consulting iced, so the walk cannot lose a
/// subtree to furniture and still balance its iced-node accounting.
fn document_node_count(node: &CompiledNode, ui: &CompiledUi, reads: &dyn Reads) -> usize {
    match node {
        CompiledNode::Split { children, .. } => {
            1 + children
                .iter()
                .map(|cell| document_node_count(&cell.node, ui, reads))
                .sum::<usize>()
        }
        CompiledNode::Module {
            chrome,
            collapsed,
            root,
            ..
        } => {
            let is_collapsed = *chrome == ChromeStyle::Full
                && matches!(
                    reads.get(ui.resolve(*collapsed)),
                    Some(ReadValue::Bool(true))
                );
            if is_collapsed {
                1
            } else {
                1 + expanded_node_count(root)
            }
        }
        _ => 1,
    }
}

fn expanded_node_count(node: &ExpandedNode) -> usize {
    match node {
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. } => {
            1 + children.iter().map(expanded_node_count).sum::<usize>()
        }
        // A reveal writes no rect of its own: the node the walk emits is the
        // one it holds.
        ExpandedNode::Reveal { child, .. } => expanded_node_count(child),
        _ => 1,
    }
}

/// Whether a cell of a measured flow stood in the room it was laid out in.
///
/// `render/tree/flex.rs` leaves `layout::Node::default()` in the place of a
/// cell the room never reached, so a cell with neither size nor children is
/// one that did not stand.
fn stood(layout: Layout<'_>) -> bool {
    let bounds = layout.bounds();
    bounds.width > 0.0 || bounds.height > 0.0 || layout.children().next().is_some()
}

fn compiled_path(node: &CompiledNode, ui: &CompiledUi, parent: &str, position: usize) -> String {
    match node {
        CompiledNode::Split { .. } => positional_path(parent, "split", position),
        CompiledNode::Module { instance, .. } => ui.resolve(*instance).to_owned(),
        _ => positional_path(parent, "compiled", position),
    }
}

fn expanded_path(node: &ExpandedNode, ui: &CompiledUi, parent: &str, position: usize) -> String {
    match node {
        ExpandedNode::Row { id, .. } => id.map_or_else(
            || positional_path(parent, "row", position),
            |id| named_path(parent, ui.resolve(id)),
        ),
        ExpandedNode::Column { id, .. } => id.map_or_else(
            || positional_path(parent, "column", position),
            |id| named_path(parent, ui.resolve(id)),
        ),
        ExpandedNode::Slot { id, .. } => named_path(parent, ui.resolve(*id)),
        ExpandedNode::Control { path, .. }
        | ExpandedNode::Popover { path, .. }
        | ExpandedNode::Pressable { path, .. } => ui.resolve(*path).to_owned(),
        _ => positional_path(parent, "expanded", position),
    }
}

fn positional_path(parent: &str, kind: &str, position: usize) -> String {
    named_path(parent, &format!("{kind}[{position}]"))
}

fn named_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn only_child<'a>(layout: Layout<'a>, path: &str, role: &str) -> Layout<'a> {
    exact_children(layout, 1, path, role)[0]
}

fn exact_children<'a>(
    layout: Layout<'a>,
    expected: usize,
    path: &str,
    role: &str,
) -> Vec<Layout<'a>> {
    let children = layout.children().collect::<Vec<_>>();
    assert_eq!(
        children.len(),
        expected,
        "layout correspondence changed at document path `{path}`: {role} expected {expected} \
         children, found {}",
        children.len()
    );
    children
}

fn layout_node_count(layout: Layout<'_>) -> usize {
    1 + layout.children().map(layout_node_count).sum::<usize>()
}

fn fixture_line_path(line: &str) -> &str {
    line.split_ascii_whitespace()
        .next()
        .unwrap_or("<end of fixture>")
}

fn assert_fixture_matches(path: &Path, expected: &str, actual: &str) {
    if let Some((line, expected_line, actual_line)) = first_difference(expected, actual) {
        assert_eq!(
            actual_line,
            expected_line,
            "layout fixture mismatch in {} at line {line}, document path `{}`\nexpected: \
             {expected_line}\nactual: {actual_line}",
            path.display(),
            fixture_line_path(actual_line),
        );
    }
}

fn first_difference<'a, 'b>(
    expected: &'a str,
    actual: &'b str,
) -> Option<(usize, &'a str, &'b str)> {
    let mut expected_lines = expected.split('\n');
    let mut actual_lines = actual.split('\n');
    let mut line = 1;
    loop {
        let expected_line = expected_lines.next();
        let actual_line = actual_lines.next();
        match (expected_line, actual_line) {
            (None, None) => return None,
            (Some(expected_line), Some(actual_line)) if expected_line == actual_line => {}
            (expected_line, actual_line) => {
                return Some((
                    line,
                    expected_line.unwrap_or("<end of fixture>"),
                    actual_line.unwrap_or("<end of fixture>"),
                ));
            }
        }
        line += 1;
    }
}

#[kithara::test]
fn builtin_layouts_match_rect_fixtures() {
    let reads = FixtureReads::default();
    let renderer = headless_renderer();
    let skin = fixture_skin();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/layout");
    let update = env::var_os("KITHARA_UI_UPDATE_LAYOUT_FIXTURES").is_some();
    if update {
        fs::create_dir_all(&fixture_dir).expect("layout fixture directory must be writable");
    }

    for preset in [builtin::MICRO_PRESET, builtin::PLAYER_PRESET] {
        let ui = compile(
            preset,
            &builtin::resolver(),
            &crate::common::registry::player_registry(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
            &view::EMPTY,
        )
        .expect("builtin layout must compile");
        let mut actual = String::new();
        // 320x240 is narrow enough to drive every fluid child below the `min`
        // its `Dim::Range` declares, which is the only region where honouring
        // that min can change a rect. Without it the Range decision would ship
        // with a measured delta of zero.
        for viewport in [
            Size::new(1280.0, 720.0),
            Size::new(960.0, 600.0),
            Size::new(320.0, 240.0),
        ] {
            actual.push_str(&dump(preset, &ui, &reads, &skin, &renderer, viewport));
        }

        let stem = preset
            .strip_suffix(".klayout.ron")
            .expect("builtin preset must use the klayout suffix");
        let fixture = fixture_dir.join(format!("{stem}.rects"));
        if update {
            fs::write(&fixture, actual).expect("layout fixture must be writable");
        } else {
            let expected =
                fs::read_to_string(&fixture).expect("layout fixture must exist and be readable");
            assert_fixture_matches(&fixture, &expected, &actual);
        }
    }
}

#[kithara::test]
fn split_weights_reach_layout_as_f32() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "fractional.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "fractional",
            root: Split(axis: Horizontal, children: [
                (weight: 0.335, node: Module(
                    instance: "fractional",
                    source: "fill.kmodule.ron",
                    size: (w: Fill, h: Fill),
                )),
                (weight: 1.0, node: Module(
                    instance: "unit",
                    source: "fill.kmodule.ron",
                    size: (w: Fill, h: Fill),
                )),
            ]))"#,
    );
    resolver.insert(
        "fill.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "fill", chrome: Plain,
            root: Row(
                id: "body",
                size: (w: Fill, h: Fill),
                gap: 0.0,
                pad: 0.0,
                children: [],
            ))"#,
    );
    let ui = compile(
        "fractional.klayout.ron",
        &resolver,
        &crate::common::registry::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .expect("fractional split document must compile");
    let actual = dump(
        "fractional.klayout.ron",
        &ui,
        &FixtureReads::default(),
        &fixture_skin(),
        &headless_renderer(),
        Size::new(1335.0, 100.0),
    );
    let module_rects = actual
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("fractional ") || line.starts_with("unit "))
        .collect::<Vec<_>>();

    assert_eq!(
        module_rects,
        [
            "fractional 0.000 0.000 335.000 100.000",
            "unit 335.000 0.000 1000.000 100.000",
        ],
        "split weights were quantized through FillPortion (the rounded result is 338.731:996.269)",
    );
}

/// A band names a room in the box the document declared, not what is left of it
/// once the flow spends its own padding. `render/masonry/flex.rs` reads the same
/// number, so a document means one thing on both hosts.
#[kithara::test]
fn a_padded_measuring_row_reads_its_bands_against_the_declared_box() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "banded.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "banded",
            root: Split(axis: Horizontal, children: [
                (weight: 1.0, node: Module(
                    instance: "bar",
                    source: "bar.kmodule.ron",
                    size: (w: Fill, h: Fill),
                )),
            ]))"#,
    );
    resolver.insert(
        "bar.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "bar", chrome: Plain,
            root: Row(
                id: "body",
                size: (w: Fill, h: Fill),
                gap: 0.0,
                pad_x: 30.0,
                measure: Width,
                children: [
                    Reveal(from: 350.0, child: Spacer(
                        id: "late",
                        size: Some((w: Fixed(30.0), h: Fill)),
                    )),
                ],
            ))"#,
    );
    let ui = compile(
        "banded.klayout.ron",
        &resolver,
        &crate::common::registry::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .expect("banded document must compile");
    let laid_out = |width: f32| {
        dump(
            "banded.klayout.ron",
            &ui,
            &FixtureReads::default(),
            &fixture_skin(),
            &headless_renderer(),
            Size::new(width, 42.0),
        )
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("bar/late ").map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("the banded cell must be named at {width} wide"))
    };

    assert_eq!(
        laid_out(349.0),
        "-",
        "349 does not reach the band, so the cell has no box"
    );
    assert_eq!(
        laid_out(360.0),
        "30.000 0.000 30.000 42.000",
        "360 reaches 350 before the row spends its padding"
    );
}

#[kithara::test]
fn committed_layout_fixture_corpus_needs_no_fallback_face() {
    let mut context = TextContext::new().unwrap();
    let text = builtin::skin_doc().text;

    for content in fixture_text_corpus() {
        for role in [text.brand, text.body, text.telemetry] {
            let run = context.shape(content, role, None);
            assert!(
                run.segments()
                    .iter()
                    .all(|segment| matches!(segment.face(), GlyphFace::Embedded(_))),
                "fixture text `{content}` reached a fallback face in {:?}",
                role.font
            );
            assert!(
                run.segments()
                    .iter()
                    .flat_map(GlyphSegment::glyphs)
                    .all(|glyph| glyph.id != 0),
                "fixture text `{content}` reached .notdef in {:?}",
                role.font
            );
        }
    }
}

/// Every box of exactly this size in the laid-out tree.
///
/// A placement is not addressable in a layout the way a widget is, so a test
/// gives the picture one box of a size nothing else has and finds it by that.
fn boxes_of(layout: Layout<'_>, size: Size) -> Vec<Rectangle> {
    let bounds = layout.bounds();
    let mut found = if bounds.width == size.width && bounds.height == size.height {
        vec![bounds]
    } else {
        Vec::new()
    };
    for child in layout.children() {
        found.extend(boxes_of(child, size));
    }
    found
}

/// Where the one box of that size stands.
///
/// A sized cell reaches iced as a container around the widget it sizes, so one
/// box of the picture is several nodes of the layout, all in the same place.
fn stands_at(layout: Layout<'_>, size: Size, role: &str) -> (f32, f32) {
    let mut found = boxes_of(layout, size)
        .into_iter()
        .map(|bounds| (bounds.x, bounds.y))
        .collect::<Vec<_>>();
    found.dedup();
    let [only] = found.as_slice() else {
        panic!("the scene must lay out one {role}, not {found:?}");
    };
    *only
}

const SCENE: &str = r#"(schema: "kithara.module", version: 1, id: "scene", chrome: Plain,
    root: Stage(id: "stage", size: (w: Fill, h: Fill), children: [
        Spacer(id: "mark", size: Some((w: Fixed(10.0), h: Fixed(10.0)))),
        Placed(id: "carry", at: (40.0, 24.0),
            read: Model(id: "scene.at"),
            write: Parameter(id: "scene.at"),
            child: Spacer(id: "sprite", size: Some((w: Fixed(40.0), h: Fixed(20.0))))),
    ]))"#;

/// Where a placement's child ended up in the scene around it, measured against
/// the marker that stands at the stage's own origin.
fn scene_placement(reads: &FixtureReads) -> (f32, f32) {
    let mut registry = crate::common::registry::player_registry();
    for category in [EndpointCategory::Model, EndpointCategory::Parameter] {
        registry.insert(category, "scene.at", EndpointDesc::new(ValueKind::Point));
    }
    let mut resolver = MemResolver::default();
    resolver.insert(
        "scene.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "scene",
            root: Module(instance: "scene", source: "scene.kmodule.ron",
                size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert("scene.kmodule.ron", SCENE);
    let ui = compile(
        "scene.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .expect("the scene document must compile");
    let skin = fixture_skin();
    let renderer = headless_renderer();
    let mut element = tree::render(
        &ui.root,
        &ui,
        reads,
        &view::EMPTY,
        &skin,
        Clock::default(),
        None,
    );
    let mut tree = Tree::new(element.as_widget());
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &Limits::new(Size::ZERO, Size::new(300.0, 200.0)),
    );
    let layout = Layout::new(&node);
    let sprite = stands_at(layout, Size::new(40.0, 20.0), "sprite");
    let stage = stands_at(layout, Size::new(10.0, 10.0), "marker");
    (sprite.0 - stage.0, sprite.1 - stage.1)
}

/// A placement moves the box its child is laid out in, so the immediate host
/// puts the child where the document says rather than drawing it there and
/// leaving the box that answers the pointer behind.
#[kithara::test]
fn a_placement_lays_its_child_out_at_the_point_the_document_wrote() {
    assert_eq!(scene_placement(&FixtureReads::default()), (40.0, 24.0));
}

/// And the point is the application's: the same compiled document lays the
/// child out where the endpoint now answers.
#[kithara::test]
fn a_placement_lays_its_child_out_at_the_point_its_endpoint_answers() {
    let reads = FixtureReads {
        placed: Some(Pt { x: 120.0, y: 60.0 }),
        ..FixtureReads::default()
    };

    assert_eq!(scene_placement(&reads), (120.0, 60.0));
}
