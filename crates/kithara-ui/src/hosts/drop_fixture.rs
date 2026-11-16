//! A library beside a deck that takes drops, shared by the gesture tests of
//! both hosts so each proves the same session.

use std::{borrow::Cow, cell::Cell, collections::BTreeMap, sync::LazyLock};

use num_traits::AsPrimitive;

use crate::{
    atoms::table::{TableMetrics, table_body, table_row_pitch},
    builtin,
    compile::{CompiledUi, compile},
    draw::Rect,
    mock::MapEndpoints,
    module::{IconName, TableFrame},
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{
        Badge, Published, ReadValue, Reads, Scope, TableCell, TableRow, TreeRow, UiEvent,
        WriteValue,
    },
    source::{MemResolver, UiConfig},
    view,
};

pub(crate) const LOAD: &str = "demo.load@deck=b";

pub(crate) const WIDTH: f32 = 400.0;
pub(crate) const HEIGHT: f32 = 200.0;

fn source(url: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("source".to_owned(), url.to_owned())])
}

fn row(title: &'static str, url: Option<&'static str>) -> TableRow<'static> {
    let row = TableRow::new(vec![TableCell::text("title", title)], false);
    match url {
        Some(url) => row.with_drag(Cow::Owned(source(url))),
        None => row,
    }
}

static LISTED: LazyLock<Vec<TableRow<'static>>> = LazyLock::new(|| {
    vec![
        row("One", Some("file:///one.mp3")),
        row("Two", Some("file:///two.mp3")),
        row("Three", Some("file:///three.mp3")),
    ]
});

static REORDERED: LazyLock<Vec<TableRow<'static>>> = LazyLock::new(|| {
    vec![
        row("Three", Some("file:///three.mp3")),
        row("One", Some("file:///one.mp3")),
        row("Two", Some("file:///two.mp3")),
    ]
});

static FILTERED: LazyLock<Vec<TableRow<'static>>> = LazyLock::new(|| {
    vec![
        row("One", Some("file:///one.mp3")),
        row("Three", Some("file:///three.mp3")),
    ]
});

static BARE: LazyLock<Vec<TableRow<'static>>> =
    LazyLock::new(|| vec![row("One", None), row("Two", None), row("Three", None)]);

static ACTIONS: LazyLock<Vec<TableRow<'static>>> = LazyLock::new(|| {
    [
        ("One", Some("urn:item:one")),
        ("Two", Some("urn:item:two")),
        ("Three", None),
    ]
    .into_iter()
    .map(|(title, action)| {
        let icon = TableCell::icon("lead", IconName::Heart, title == "Two");
        let icon = match action {
            Some(action) => icon.with_action(action),
            None => icon,
        };
        TableRow::new(vec![icon, TableCell::text("title", title)], title == "Two")
            .with_drag(Cow::Owned(source("file:///dragged.mp3")))
    })
    .collect()
});

static ACTIONS_REORDERED: LazyLock<Vec<TableRow<'static>>> =
    LazyLock::new(|| vec![ACTIONS[1].clone(), ACTIONS[0].clone(), ACTIONS[2].clone()]);

static PLAYING: [Badge<'static>; 1] = [Badge {
    label: "A",
    active: true,
}];

static LOADED: [Badge<'static>; 1] = [Badge {
    label: "A",
    active: false,
}];

static BADGED: LazyLock<Vec<TableRow<'static>>> = LazyLock::new(|| {
    [&PLAYING[..], &LOADED[..], &[]]
        .into_iter()
        .map(|badges| {
            TableRow::new(
                vec![
                    TableCell::badges("lead", badges),
                    TableCell::text("title", "Track"),
                ],
                false,
            )
        })
        .collect()
});

static TREE: [TreeRow<'static>; 3] = [
    TreeRow {
        label: "Explorer",
        icon: IconName::Folder,
        count: None,
        expanded: Some(true),
        page: false,
        muted: false,
        selected: false,
        depth: 0,
    },
    TreeRow {
        label: "Music",
        icon: IconName::Folder,
        count: Some(6),
        expanded: Some(false),
        page: true,
        muted: false,
        selected: false,
        depth: 1,
    },
    TreeRow {
        label: "notes",
        icon: IconName::Folder,
        count: None,
        expanded: None,
        page: true,
        muted: false,
        selected: false,
        depth: 1,
    },
];

/// [`TREE`] once its folder row is open: one more chevron stands below it.
static OPENED_TREE: [TreeRow<'static>; 4] = [
    TREE[0],
    TreeRow {
        expanded: Some(true),
        ..TREE[1]
    },
    TreeRow {
        label: "Live",
        icon: IconName::Folder,
        count: None,
        expanded: Some(false),
        page: true,
        muted: false,
        selected: false,
        depth: 2,
    },
    TREE[2],
];

pub(crate) const DRAGGED: &str = "file:///two.mp3";
pub(crate) const DRAGGED_TITLE: &str = "Two";
pub(crate) const FILTERED_IN_ITS_PLACE: &str = "file:///three.mp3";
pub(crate) const REORDERED_IN_ITS_PLACE: &str = "file:///one.mp3";

#[derive(Clone, Copy)]
pub(crate) enum Rows {
    Listed,
    Filtered,
    Reordered,
    Bare,
    Badged,
    Actions,
    ActionsReordered,
}

pub(crate) struct DropReads {
    pub(crate) rows: Cell<Rows>,
    /// The tree reads as [`OPENED_TREE`].
    pub(crate) opened: Cell<bool>,
}

impl DropReads {
    pub(crate) const fn new(rows: Rows) -> Self {
        Self {
            rows: Cell::new(rows),
            opened: Cell::new(false),
        }
    }
}

impl Reads for DropReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = Scope::split(endpoint).0;
        match id {
            "library.visible_tracks" => Some(ReadValue::Table(match self.rows.get() {
                Rows::Listed => &LISTED[..],
                Rows::Filtered => &FILTERED[..],
                Rows::Reordered => &REORDERED[..],
                Rows::Bare => &BARE[..],
                Rows::Badged => &BADGED[..],
                Rows::Actions => &ACTIONS[..],
                Rows::ActionsReordered => &ACTIONS_REORDERED[..],
            })),
            "library.browser_open" => Some(ReadValue::Bool(true)),
            "library.tree" => Some(ReadValue::Tree(if self.opened.get() {
                &OPENED_TREE
            } else {
                &TREE
            })),
            "library.query" => Some(ReadValue::Text("")),
            _ => None,
        }
    }
}

const LISTED_IN_PLACE: &str = r#"(schema: "kithara.module", version: 1, id: "library",
    root: Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
        Table(
            id: "tracks",
            size: (w: Fill, h: Fill),
            read: Model(id: "library.visible_tracks"),
            columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)],
        ),
    ]))"#;

const LISTED_ON_A_POPOVER: &str = r#"(schema: "kithara.module", version: 1, id: "library",
    root: Popover(
        id: "browser",
        open: Model(id: "library.browser_open"),
        anchor: Spacer(id: "strip", size: (w: Fill, h: Fixed(20.0))),
        content: Table(
            id: "tracks",
            size: (w: Fixed(180.0), h: Fixed(160.0)),
            read: Model(id: "library.visible_tracks"),
            columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)],
        ),
    ))"#;

pub(crate) const RESIZED: &str = "library/tracks/width/title";
pub(crate) const BORDER: (f32, f32) = (100.0, 10.0);
pub(crate) const WIDER: (f32, f32) = (130.0, 10.0);

fn two_columns(write_width: &str) -> String {
    format!(
        r#"(schema: "kithara.module", version: 1, id: "library",
    root: Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
        Table(
            id: "tracks",
            size: (w: Fill, h: Fill),
            read: Model(id: "library.visible_tracks"),{write_width}
            columns: [
                (id: "title", label: "TITLE", style: Primary, width: 100.0),
                (id: "artist", label: "ARTIST", style: Secondary, width: 80.0),
            ],
        ),
    ]))"#
    )
}

const LIBRARY_WIDTH: f32 = 200.0;
const LEAD_WIDTH: f32 = 30.0;

fn badge_columns() -> String {
    format!(
        r#"(schema: "kithara.module", version: 1, id: "library",
    root: Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
        Table(
            id: "tracks",
            size: (w: Fill, h: Fill),
            read: Model(id: "library.visible_tracks"),
            columns: [
                (id: "lead", label: "", style: Badge, width: {LEAD_WIDTH:.1}),
                (id: "title", label: "TITLE", style: Primary, width: 170.0),
            ],
        ),
    ]))"#
    )
}

pub(crate) fn lead_cell(photo: &[u8], row: u8) -> Vec<u8> {
    let skin = builtin::skin();
    let body = table_body(
        Rect {
            x: 0.0,
            y: 0.0,
            w: LIBRARY_WIDTH,
            h: HEIGHT,
        },
        TableMetrics {
            skin: &skin.table,
            frame: TableFrame::new(0.0, 0.0, true),
        },
    );
    let top = f32::from(row).mul_add(table_row_pitch(&skin.table), body.y);
    let stride: usize = WIDTH.as_();
    let lead: usize = LEAD_WIDTH.as_();
    let rows: std::ops::Range<usize> = top.as_()..(top + skin.table.row_height).as_();
    rows.flat_map(|y| {
        photo[y * stride * 4..(y * stride + lead) * 4]
            .iter()
            .copied()
    })
    .collect()
}

/// How the tree fixture declares its `Tree`, and which host owns its input.
#[derive(Clone, Copy)]
pub(crate) struct TreeDoc {
    pub(crate) engine: bool,
    pub(crate) query: bool,
    pub(crate) toggle: bool,
}

pub(crate) const SELECT: &str = "demo.select";
pub(crate) const TOGGLE: &str = "demo.toggle";
const ACTION: &str = "demo.action";

const ACTION_TABLE: &str = r#"(schema: "kithara.module", version: 1, id: "app-library",
    root: Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
        Table(
            id: "tracks", size: (w: Fill, h: Fill),
            read: Model(id: "library.visible_tracks"),
            write: Command(id: "demo.select"),
            columns: [
                (id: "lead", label: "", style: Icon, width: 28.0,
                    write: Command(id: "demo.action")),
                (id: "title", label: "TITLE", style: Primary, width: 172.0, flexible: true),
            ],
        ),
    ]))"#;

fn tree_module(doc: TreeDoc) -> String {
    let id = if doc.engine { "app-library" } else { "library" };
    let query = if doc.query {
        r#"
            query: Model(id: "library.query"),"#
    } else {
        ""
    };
    let toggle = if doc.toggle {
        r#"
            toggle: Command(id: "demo.toggle"),"#
    } else {
        ""
    };
    format!(
        r#"(schema: "kithara.module", version: 1, id: "{id}",
    root: Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
        Tree(
            id: "browser",
            size: (w: Fill, h: Fill),
            read: Model(id: "library.tree"),
            write: Command(id: "demo.select"),{query}{toggle}
        ),
    ]))"#
    )
}

pub(crate) fn compiled() -> CompiledUi {
    compiled_with(LISTED_IN_PLACE)
}

pub(crate) fn compiled_with_badges() -> CompiledUi {
    compiled_with(&badge_columns())
}

pub(crate) fn compiled_tree(doc: TreeDoc) -> CompiledUi {
    compiled_with(&tree_module(doc))
}

pub(crate) fn compiled_engine_hosted() -> CompiledUi {
    compiled_with(&LISTED_IN_PLACE.replacen(r#"id: "library""#, r#"id: "app-library""#, 1))
}

pub(crate) fn compiled_on_a_popover() -> CompiledUi {
    compiled_with(LISTED_ON_A_POPOVER)
}

pub(crate) fn compiled_with_columns() -> CompiledUi {
    compiled_with(&two_columns(""))
}

pub(crate) fn compiled_with_column_widths() -> CompiledUi {
    compiled_with(&two_columns(
        r#"
            write_width: Parameter(id: "demo.width"),"#,
    ))
}

fn compiled_with(library: &str) -> CompiledUi {
    let mut registry = MapEndpoints::default();
    registry.insert(
        EndpointCategory::Model,
        "library.visible_tracks",
        EndpointDesc::new(ValueKind::Table),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.browser_open",
        EndpointDesc::new(ValueKind::Bool),
    );
    registry.insert(
        EndpointCategory::Parameter,
        "demo.width",
        EndpointDesc::new(ValueKind::Scalar).with_scope("column"),
    );
    registry.insert(
        EndpointCategory::Command,
        "demo.load",
        EndpointDesc::new(ValueKind::Record).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.tree",
        EndpointDesc::new(ValueKind::Tree),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.query",
        EndpointDesc::new(ValueKind::Text),
    );
    for id in [SELECT, TOGGLE] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Index),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        ACTION,
        EndpointDesc::new(ValueKind::Text),
    );
    let mut resolver = MemResolver::default();
    resolver.insert(
        "fixture.klayout.ron",
        &format!(
            r#"(schema: "kithara.layout", version: 1, id: "fixture",
            root: Split(axis: Horizontal, children: [
                (node: Module(instance: "library", source: "library.kmodule.ron",
                    size: (w: Fixed({LIBRARY_WIDTH:.1}), h: Fill))),
                (node: Module(instance: "deck-b", source: "deck.kmodule.ron", with: {{ "deck": "b" }},
                    size: (w: Fill, h: Fill))),
            ]))"#
        ),
    );
    resolver.insert("library.kmodule.ron", library);
    resolver.insert(
        "deck.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "deck",
            parameters: ["deck"],
            drop: Some((write: Command(id: "demo.load", with: { "deck": "$deck" }))),
            root: Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
                Text(id: "name", style: MicroLabel, label: "DECK", size: (w: Fill, h: Fill)),
            ]))"#,
    );
    compile(
        "fixture.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the drop fixture must compile: {error}"))
}

pub(crate) const FROM: (f32, f32) = (60.0, 60.0);
pub(crate) const CARRY: [(f32, f32); 2] = [(64.0, 62.0), (150.0, 80.0)];
pub(crate) const OVER_THE_DECK: (f32, f32) = (300.0, 100.0);
pub(crate) const AWAY: (f32, f32) = (100.0, 150.0);
pub(crate) const ZONE: &str = "deck-b/drop";

pub(crate) fn writes(ui: &CompiledUi, published: &[Published], reads: &dyn Reads) -> Vec<UiEvent> {
    let mut view = view::ViewState::default();
    published
        .iter()
        .filter(|event| matches!(event, Published::Gesture { .. }))
        .filter_map(|event| ui.views().settle(event.clone(), reads, &mut view))
        .collect()
}

pub(crate) fn load(url: &str) -> UiEvent {
    UiEvent::Write {
        key: LOAD.to_owned(),
        value: WriteValue::Record(source(url)),
    }
}

fn index(key: &str, row: usize) -> UiEvent {
    UiEvent::Write {
        key: key.to_owned(),
        value: WriteValue::Index(row),
    }
}

pub(crate) trait DropHost {
    fn carry_to(&mut self, ui: &CompiledUi, reads: &DropReads, at: (f32, f32));
    fn let_go(&mut self, ui: &CompiledUi, reads: &DropReads, at: (f32, f32)) -> Vec<Published>;
    fn open(ui: &CompiledUi, reads: &DropReads) -> Self;
    fn pick_up(&mut self, ui: &CompiledUi, reads: &DropReads, at: (f32, f32));
    fn refresh(&mut self, ui: &CompiledUi, reads: &DropReads);
}

fn carried<H: DropHost>(host: &mut H, ui: &CompiledUi, reads: &DropReads, from: (f32, f32)) {
    host.pick_up(ui, reads, from);
    for at in CARRY {
        host.carry_to(ui, reads, at);
    }
    host.carry_to(ui, reads, OVER_THE_DECK);
}

fn let_go_over_the_deck<H: DropHost>(
    host: &mut H,
    ui: &CompiledUi,
    reads: &DropReads,
) -> Vec<UiEvent> {
    let published = host.let_go(ui, reads, OVER_THE_DECK);
    writes(ui, &published, reads)
}

fn dropped_from<H: DropHost>(ui: &CompiledUi, rows: Rows, from: (f32, f32)) -> Vec<UiEvent> {
    let reads = DropReads::new(rows);
    let mut host = H::open(ui, &reads);
    carried(&mut host, ui, &reads, from);
    let_go_over_the_deck(&mut host, ui, &reads)
}

fn dropped_after<H: DropHost>(rows: Rows) -> (Vec<UiEvent>, Vec<UiEvent>) {
    let ui = compiled();
    let reads = DropReads::new(Rows::Listed);
    let mut host = H::open(&ui, &reads);

    carried(&mut host, &ui, &reads, FROM);
    reads.rows.set(rows);
    host.refresh(&ui, &reads);
    let writes = let_go_over_the_deck(&mut host, &ui, &reads);
    carried(&mut host, &ui, &reads, FROM);
    (writes, let_go_over_the_deck(&mut host, &ui, &reads))
}

pub(crate) fn a_row_dropped_on_a_deck_delivers_the_decks_write_with_the_rows_drag_data<
    H: DropHost,
>() {
    let writes = dropped_from::<H>(&compiled(), Rows::Listed, FROM);

    assert_eq!(writes, [load(DRAGGED)]);
}

pub(crate) fn table_icon_actions_capture_ids_before_the_rows_change<H: DropHost>() {
    let ui = compiled_with(ACTION_TABLE);
    let reads = DropReads::new(Rows::Actions);
    let mut host = H::open(&ui, &reads);
    let skin = builtin::skin();
    let y = skin.table.header_height + skin.table.grid_gap + skin.table.row_height / 2.0;
    let at = (14.0, y);
    host.pick_up(&ui, &reads, at);
    host.carry_to(&ui, &reads, OVER_THE_DECK);
    reads.rows.set(Rows::ActionsReordered);
    host.refresh(&ui, &reads);
    let first = writes(&ui, &host.let_go(&ui, &reads, OVER_THE_DECK), &reads);
    assert_eq!(
        first,
        [UiEvent::Write {
            key: ACTION.to_owned(),
            value: WriteValue::Text("urn:item:one".to_owned()),
        }]
    );
    host.pick_up(&ui, &reads, at);
    let second = writes(&ui, &host.let_go(&ui, &reads, at), &reads);
    assert_eq!(
        second,
        [UiEvent::Write {
            key: ACTION.to_owned(),
            value: WriteValue::Text("urn:item:two".to_owned()),
        }]
    );
    let no_action = (14.0, y + table_row_pitch(&skin.table) * 2.0);
    host.pick_up(&ui, &reads, no_action);
    let third = writes(&ui, &host.let_go(&ui, &reads, no_action), &reads);
    assert_eq!(third, [index(SELECT, 2)]);
    host.pick_up(&ui, &reads, (60.0, y));
    let title = writes(&ui, &host.let_go(&ui, &reads, (60.0, y)), &reads);
    assert_eq!(title, [index(SELECT, 0)]);
}

pub(crate) fn a_row_dropped_after_the_list_was_filtered_delivers_the_row_that_was_dragged<
    H: DropHost,
>() {
    let (writes, after) = dropped_after::<H>(Rows::Filtered);

    assert_eq!(
        after,
        [load(FILTERED_IN_ITS_PLACE)],
        "the list must show the filtered rows by the time of the drop, or the drop proves nothing"
    );
    assert_eq!(writes, [load(DRAGGED)]);
}

pub(crate) fn a_row_dropped_after_the_list_was_reordered_delivers_the_row_that_was_dragged<
    H: DropHost,
>() {
    let (writes, after) = dropped_after::<H>(Rows::Reordered);

    assert_eq!(
        after,
        [load(REORDERED_IN_ITS_PLACE)],
        "the list must show the re-ordered rows by the time of the drop, or the drop proves nothing"
    );
    assert_eq!(writes, [load(DRAGGED)]);
}

pub(crate) fn a_row_without_drag_data_delivers_nothing_when_dropped<H: DropHost>() {
    let writes = dropped_from::<H>(&compiled(), Rows::Bare, FROM);

    assert_eq!(writes, []);
}

pub(crate) fn a_row_carried_into_a_zone_and_let_go_outside_it_delivers_nothing<H: DropHost>() {
    let ui = compiled();
    let reads = DropReads::new(Rows::Listed);
    let mut host = H::open(&ui, &reads);

    carried(&mut host, &ui, &reads, FROM);
    host.carry_to(&ui, &reads, AWAY);
    let published = host.let_go(&ui, &reads, AWAY);

    assert_eq!(writes(&ui, &published, &reads), []);
}

pub(crate) fn a_row_dropped_from_a_list_on_a_popover_delivers_the_decks_write<H: DropHost>() {
    const STRIP: f32 = 20.0;
    const FROM_THE_POPOVER: (f32, f32) = (FROM.0, FROM.1 + STRIP);
    let writes = dropped_from::<H>(&compiled_on_a_popover(), Rows::Listed, FROM_THE_POPOVER);

    assert_eq!(writes, [load(DRAGGED)]);
}

/// The tree row that has a chevron below the top level.
const FOLDER: u8 = 1;

fn tree_row_y(row: u8, searched: bool) -> f32 {
    let skin = builtin::skin();
    let search = if searched {
        skin.tree.search_height
    } else {
        0.0
    };
    search + skin.tree.panel_padding_top + skin.tree.row_height * (f32::from(row) + 0.5)
}

fn chevron(row: u8, searched: bool) -> (f32, f32) {
    let skin = builtin::skin();
    let depth = TREE[usize::from(row)].depth;
    let x = skin.tree.indent_step.mul_add(
        f32::from(depth),
        skin.tree.marker_width + skin.tree.indent_base,
    ) + skin.tree.chevron_width / 2.0;
    (x, tree_row_y(row, searched))
}

fn label(row: u8, searched: bool) -> (f32, f32) {
    (WIDTH / 4.0, tree_row_y(row, searched))
}

fn pressed<H: DropHost>(doc: TreeDoc, at: (f32, f32)) -> Vec<UiEvent> {
    let ui = compiled_tree(doc);
    let reads = DropReads::new(Rows::Listed);
    let mut host = H::open(&ui, &reads);
    host.pick_up(&ui, &reads, at);
    let published = host.let_go(&ui, &reads, at);
    writes(&ui, &published, &reads)
}

pub(crate) fn pressing_each_chevron_delivers_the_toggle_write_with_its_own_row<H: DropHost>(
    engine: bool,
) {
    let doc = TreeDoc {
        engine,
        query: true,
        toggle: true,
    };

    for row in [0, FOLDER] {
        assert_eq!(
            pressed::<H>(doc, chevron(row, true)),
            [index(TOGGLE, usize::from(row))],
            "the chevron of row {row}"
        );
    }
}

pub(crate) fn pressing_a_chevron_again_once_its_folder_opened_delivers_the_toggle_again<
    H: DropHost,
>(
    engine: bool,
) {
    let ui = compiled_tree(TreeDoc {
        engine,
        query: true,
        toggle: true,
    });
    let reads = DropReads::new(Rows::Listed);
    let mut host = H::open(&ui, &reads);
    let at = chevron(FOLDER, true);
    host.pick_up(&ui, &reads, at);
    let first = writes(&ui, &host.let_go(&ui, &reads, at), &reads);
    reads.opened.set(true);
    host.refresh(&ui, &reads);
    host.pick_up(&ui, &reads, at);
    let second = writes(&ui, &host.let_go(&ui, &reads, at), &reads);

    let toggle = index(TOGGLE, usize::from(FOLDER));
    assert_eq!(first, std::slice::from_ref(&toggle));
    assert_eq!(second, [toggle]);
}

pub(crate) fn pressing_a_label_delivers_the_select_write_with_its_row<H: DropHost>(engine: bool) {
    let doc = TreeDoc {
        engine,
        query: true,
        toggle: true,
    };

    assert_eq!(
        pressed::<H>(doc, label(FOLDER, true)),
        [index(SELECT, usize::from(FOLDER))]
    );
}

pub(crate) fn a_tree_without_toggle_selects_from_its_chevron<H: DropHost>(engine: bool) {
    let doc = TreeDoc {
        engine,
        query: true,
        toggle: false,
    };

    assert_eq!(
        pressed::<H>(doc, chevron(FOLDER, true)),
        [index(SELECT, usize::from(FOLDER))]
    );
}

pub(crate) fn a_tree_without_query_draws_no_search_row<H: DropHost>(engine: bool) {
    let doc = TreeDoc {
        engine,
        query: false,
        toggle: true,
    };

    assert_eq!(
        pressed::<H>(doc, label(0, false)),
        [index(TOGGLE, 0)],
        "the first row must stand where the search row would"
    );
    assert_eq!(
        pressed::<H>(doc, chevron(FOLDER, false)),
        [index(TOGGLE, usize::from(FOLDER))]
    );
}

pub(crate) fn a_chevron_released_outside_its_row_does_not_toggle<H: DropHost>(engine: bool) {
    let ui = compiled_tree(TreeDoc {
        engine,
        query: true,
        toggle: true,
    });
    let reads = DropReads::new(Rows::Listed);
    let mut host = H::open(&ui, &reads);
    host.pick_up(&ui, &reads, chevron(FOLDER, true));
    assert!(writes(&ui, &host.let_go(&ui, &reads, (390.0, 190.0)), &reads).is_empty());
}

macro_rules! tree_suite {
    ($host:ty, $engine:expr) => {
        #[kithara::test]
        fn a_chevron_released_outside_its_row_does_not_toggle() {
            $crate::hosts::drop_fixture::a_chevron_released_outside_its_row_does_not_toggle::<$host>($engine);
        }
        #[kithara::test]
        fn pressing_each_chevron_delivers_the_toggle_write_with_its_own_row() {
            $crate::hosts::drop_fixture::pressing_each_chevron_delivers_the_toggle_write_with_its_own_row::<
                $host,
            >($engine);
        }

        #[kithara::test]
        fn pressing_a_chevron_again_once_its_folder_opened_delivers_the_toggle_again() {
            $crate::hosts::drop_fixture::pressing_a_chevron_again_once_its_folder_opened_delivers_the_toggle_again::<
                $host,
            >($engine);
        }

        #[kithara::test]
        fn pressing_a_label_delivers_the_select_write_with_its_row() {
            $crate::hosts::drop_fixture::pressing_a_label_delivers_the_select_write_with_its_row::<
                $host,
            >($engine);
        }

        #[kithara::test]
        fn a_tree_without_toggle_selects_from_its_chevron() {
            $crate::hosts::drop_fixture::a_tree_without_toggle_selects_from_its_chevron::<$host>(
                $engine,
            );
        }

        #[kithara::test]
        fn a_tree_without_query_draws_no_search_row() {
            $crate::hosts::drop_fixture::a_tree_without_query_draws_no_search_row::<$host>(
                $engine,
            );
        }
    };
}

pub(crate) use tree_suite;

macro_rules! drop_suite {
    ($host:ty) => {
        #[kithara::test]
        fn table_icon_actions_capture_ids_before_the_rows_change() {
            $crate::hosts::drop_fixture::table_icon_actions_capture_ids_before_the_rows_change::<$host>();
        }

        #[kithara::test]
        fn a_row_dropped_on_a_deck_delivers_the_decks_write_with_the_rows_drag_data() {
            $crate::hosts::drop_fixture::a_row_dropped_on_a_deck_delivers_the_decks_write_with_the_rows_drag_data::<$host>();
        }

        #[kithara::test]
        fn a_row_dropped_after_the_list_was_filtered_delivers_the_row_that_was_dragged() {
            $crate::hosts::drop_fixture::a_row_dropped_after_the_list_was_filtered_delivers_the_row_that_was_dragged::<$host>();
        }

        #[kithara::test]
        fn a_row_dropped_after_the_list_was_reordered_delivers_the_row_that_was_dragged() {
            $crate::hosts::drop_fixture::a_row_dropped_after_the_list_was_reordered_delivers_the_row_that_was_dragged::<$host>();
        }

        #[kithara::test]
        fn a_row_without_drag_data_delivers_nothing_when_dropped() {
            $crate::hosts::drop_fixture::a_row_without_drag_data_delivers_nothing_when_dropped::<
                $host,
            >();
        }

        #[kithara::test]
        fn a_row_carried_into_a_zone_and_let_go_outside_it_delivers_nothing() {
            $crate::hosts::drop_fixture::a_row_carried_into_a_zone_and_let_go_outside_it_delivers_nothing::<$host>();
        }

        #[kithara::test]
        fn a_row_dropped_from_a_list_on_a_popover_delivers_the_decks_write() {
            $crate::hosts::drop_fixture::a_row_dropped_from_a_list_on_a_popover_delivers_the_decks_write::<$host>();
        }
    };
}

pub(crate) use drop_suite;
