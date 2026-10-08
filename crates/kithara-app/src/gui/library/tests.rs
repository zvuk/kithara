use std::{cell::RefCell, rc::Rc};

use ::kithara::ui::{
    ids::SourceUri,
    render::{TableRow, TreeRow},
    text::{TextDoc, parse_text},
};
use kithara_app_library::{LibrarySource, PageStatus, Playable};
use kithara_test_utils::kithara;

use super::{Library, StartupSource, track::display_name};
use crate::gui::test_fixture::{Calls, Probe};

fn drag_source(row: &TableRow<'_>) -> Option<String> {
    Playable::try_from(row.drag()?.clone())
        .ok()
        .map(|track| track.source)
}

/// The page states a catalog must word for the shell to mount.
const STATUSES: [(&str, &str); 3] = [
    ("library.status.loading", "Loading"),
    ("library.status.empty", "No playable files"),
    ("library.status.error", "This folder cannot be read"),
];

fn catalog(entries: &[(&str, &str)]) -> TextDoc {
    let entries: Vec<String> = entries
        .iter()
        .map(|(key, words)| format!("{key:?}: {words:?}"))
        .collect();
    parse_text(
        &format!(
            r#"(id: "library-test", schema: "kithara.text", version: 1, entries: {{ {} }})"#,
            entries.join(", "),
        ),
        &SourceUri("library-test.ktext.ron".to_owned()),
    )
    .expect("the test catalog parses")
}

fn words() -> TextDoc {
    let labels = [
        ("library.source.collection", "Collection"),
        ("library.source.startup", "Startup"),
        ("probe.label", "Probe"),
    ];
    catalog(&[labels.as_slice(), STATUSES.as_slice()].concat())
}

fn library() -> (Library, Rc<RefCell<Calls>>) {
    let (probe, selected) = Probe::registered("probe.label");
    let registered = vec![StartupSource::registered(Vec::new()), probe];
    let mut library = Library::new(registered, &words()).expect("every label and state is worded");
    library.toggle(2);
    selected.borrow_mut().expanded.clear();
    (library, selected)
}

fn labels(library: &Library) -> Vec<(u8, &str)> {
    library
        .tree()
        .into_iter()
        .map(|row| (row.depth, row.label))
        .collect()
}

fn selected<'a>(tree: &[TreeRow<'a>]) -> Vec<&'a str> {
    tree.iter()
        .filter(|row| row.selected)
        .map(|row| row.label)
        .collect()
}

fn shown(library: &Library) -> [Option<bool>; 2] {
    ["startup", Probe::ID].map(|id| showing(library, id))
}

fn showing(library: &Library, id: &str) -> Option<bool> {
    library.index_of(id).map(|_| library.page() == Some(id))
}

#[kithara::test]
fn toggling_a_node_keeps_the_selection_and_the_shown_page() {
    let (mut library, calls) = library();
    library.select(2);
    let before = calls.borrow().selected.len();

    library.toggle(2);
    assert_eq!(
        labels(&library),
        [
            (0, "Collection"),
            (1, "Startup"),
            (0, "Probe"),
            (1, "crate"),
            (1, "leaf"),
        ],
    );
    library.toggle(3);
    assert_eq!(
        labels(&library),
        [
            (0, "Collection"),
            (1, "Startup"),
            (0, "Probe"),
            (1, "crate"),
            (2, "digger"),
            (1, "leaf"),
        ],
    );

    assert_eq!(selected(&library.tree()), ["Probe"]);
    assert_eq!(shown(&library), [Some(false), Some(true)]);
    assert_eq!(
        calls.borrow().selected.len(),
        before,
        "a toggle selects nothing"
    );

    library.toggle(2);
    assert_eq!(
        labels(&library),
        [(0, "Collection"), (1, "Startup"), (0, "Probe")]
    );
    assert_eq!(selected(&library.tree()), ["Probe"]);
}

#[kithara::test]
fn selecting_a_node_shows_only_its_source_page_and_tells_that_source() {
    let (mut library, calls) = library();
    assert_eq!(shown(&library), [Some(true), Some(false)]);

    library.toggle(2);
    library.select(3);

    assert_eq!(selected(&library.tree()), ["crate"]);
    assert_eq!(shown(&library), [Some(false), Some(true)]);
    assert_eq!(calls.borrow().selected, ["crate"]);
    assert_eq!(showing(&library, "elsewhere"), None);

    library.select(1);
    assert_eq!(shown(&library), [Some(true), Some(false)]);
    assert_eq!(
        calls.borrow().selected,
        ["crate"],
        "the probe was not selected again"
    );
}

#[kithara::test]
fn expanding_a_node_tells_its_source_and_collapsing_does_not() {
    let (mut library, calls) = library();

    library.toggle(2);
    library.toggle(3);
    library.toggle(3);
    library.toggle(2);

    assert_eq!(calls.borrow().expanded, [Probe::ID, "crate"]);
    assert!(
        calls.borrow().selected.is_empty(),
        "an expand selects nothing"
    );
}

#[kithara::test]
fn a_page_status_the_catalog_does_not_word_refuses_the_shell() {
    for (missing, _) in STATUSES {
        let entries: Vec<(&str, &str)> = STATUSES
            .into_iter()
            .filter(|(key, _)| *key != missing)
            .chain([
                ("library.source.collection", "Collection"),
                ("library.source.startup", "Startup"),
            ])
            .collect();
        let registered = vec![StartupSource::registered(Vec::new())];

        assert!(
            Library::new(registered, &catalog(&entries)).is_err(),
            "a catalog without `{missing}` mounts no shell"
        );
    }
}

#[kithara::test]
fn a_source_whose_row_the_catalog_does_not_word_is_refused() {
    let (probe, _) = Probe::registered("probe.unworded");

    assert!(Library::new(vec![probe], &words()).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test]
fn explorer_refuses_a_catalog_missing_any_of_its_labels() {
    let labels = [
        ("library.source.explorer", "Explorer"),
        ("library.node.home", "Home"),
        ("library.node.music_folders", "Music Folders"),
    ];
    for (missing, _) in labels {
        let worded: Vec<(&str, &str)> = labels
            .into_iter()
            .filter(|(key, _)| *key != missing)
            .collect();
        let catalog = catalog(&[worded.as_slice(), STATUSES.as_slice()].concat());
        let (explorer, _) =
            super::Explorer::registered(None, crate::gui::test_fixture::runtime().handle().clone());

        assert!(
            explorer.build(&catalog).is_err(),
            "a catalog without `{missing}` builds Explorer"
        );
    }
}

#[kithara::test]
fn a_title_is_the_last_segment_without_its_extension() {
    assert_eq!(display_name("https://host/path/Song 1.flac"), "Song 1");
    assert_eq!(display_name("/music/track.mp3"), "track");
    assert_eq!(display_name("noslash"), "noslash");
}

#[kithara::test]
fn library_rows_are_unique_playlist_entries_without_deck_cells() {
    let mut source = StartupSource::new(
        ["/music/a.mp3", "/music/b.mp3", "/music/a.mp3"]
            .map(str::to_owned)
            .to_vec(),
        &words(),
    )
    .expect("the startup list is worded");
    source.select("startup");

    let rows = source.rows(None);

    let drags: Vec<Option<String>> = rows.iter().map(drag_source).collect();
    assert_eq!(
        drags,
        ["/music/a.mp3", "/music/b.mp3"].map(|url| Some(url.to_owned()))
    );
    assert!(
        rows.iter()
            .all(|row| row.cells().iter().all(|cell| cell.id() != "deck"))
    );
}

#[kithara::test]
fn a_startup_list_without_tracks_reports_empty() {
    let mut source = StartupSource::new(Vec::new(), &words()).expect("the startup list is worded");
    source.select("startup");

    assert_eq!(source.status(), PageStatus::Empty);
}

#[cfg(not(feature = "broadcast"))]
mod startup {
    use std::convert::Infallible;

    use ::kithara::ui::render::{ReadValue, Reads, TableCell, TableRow, TableValue, Walk};
    use kithara_test_utils::{kithara, off_thread::OffThread};

    use crate::{
        catalog::canonical_source,
        gui::{reads::ReadRoot, rig::Rig},
    };

    /// One row of a source's page, as the page reads it.
    #[derive(Debug)]
    pub(super) struct Listed {
        pub(super) drag: Option<String>,
        pub(super) title: Option<String>,
        artist_unknown: bool,
        pub(super) selected: bool,
    }

    pub(super) fn listed(rig: &Rig, source: &str) -> Vec<Listed> {
        let root = ReadRoot::new(&rig.ui);
        let reads = Walk::new(&root);
        let key = format!("source.rows@source={source}");
        let Some(ReadValue::Table(rows)) = reads.get(&key) else {
            panic!("`{source}` answers its rows");
        };
        rows.iter().map(row).collect()
    }

    fn row(row: &TableRow<'_>) -> Listed {
        let cell = |id: &str| {
            row.cells()
                .iter()
                .find(|cell| cell.id() == id)
                .map(TableCell::value)
        };
        let title = match cell("title") {
            Some(TableValue::Text(title)) => Some(title.to_string()),
            _ => None,
        };
        Listed {
            title,
            drag: super::drag_source(row),
            selected: row.selected(),
            artist_unknown: matches!(cell("artist"), None | Some(TableValue::Empty)),
        }
    }

    pub(super) fn tree(rig: &Rig) -> Vec<(u8, String)> {
        let root = ReadRoot::new(&rig.ui);
        let reads = Walk::new(&root);
        let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
            panic!("the library draws its tree");
        };
        rows.iter()
            .map(|row| (row.depth, row.label.to_owned()))
            .collect()
    }

    const TRACKS: [&str; 2] = ["/music/local.flac", "https://example.test/stream.m3u8"];

    #[kithara::test(native, flash(false))]
    fn the_startup_list_opens_under_collection_in_the_order_given_with_streams() {
        let rig = Rig::offline();

        assert_eq!(
            tree(&rig),
            [
                (0, "Collection".to_owned()),
                (1, "Startup".to_owned()),
                (0, "Explorer".to_owned()),
                (1, "Music Folders".to_owned()),
                (1, "Home".to_owned()),
            ]
        );
        assert_eq!(rig.text("library.page").as_deref(), Some("startup"));
        let drags: Vec<Option<String>> = listed(&rig, "startup")
            .into_iter()
            .map(|row| row.drag)
            .collect();
        assert_eq!(drags, TRACKS.map(|url| Some(url.to_owned())));
    }

    #[kithara::test(native, flash(false))]
    fn no_row_shows_a_raw_url_as_artist() {
        let rig = Rig::offline();

        let rows = listed(&rig, "startup");
        assert_eq!(
            rows.iter()
                .map(|row| row.title.as_deref())
                .collect::<Vec<_>>(),
            [Some("local"), Some("stream")],
        );
        assert!(
            rows.iter().all(|row| row.artist_unknown),
            "the artist is unknown, never the url: {rows:?}"
        );
    }

    pub(super) async fn with_rig(check: impl FnOnce(&mut Rig) + Send + 'static) {
        let rig = OffThread::spawn("app-host", || Ok::<_, Infallible>(Rig::offline()))
            .await
            .expect("rig fixture is infallible");
        rig.call(check).await;
        rig.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn a_startup_stream_dragged_onto_deck_b_loads_there() {
        with_rig(|rig| {
            let drag = listed(rig, "startup")
                .into_iter()
                .find_map(|row| row.drag.filter(|drag| drag.contains("m3u8")))
                .expect("the startup branch lists the stream");

            rig.drop_on("b", &drag);

            let queued: Vec<Option<String>> = rig.queues[1]
                .tracks()
                .into_iter()
                .map(|track| track.url)
                .collect();
            assert_eq!(queued, [Some(canonical_source(&drag))]);
            assert!(
                rig.queues[0].tracks().is_empty(),
                "deck A was not the target"
            );
        })
        .await;
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(feature = "broadcast")))]
mod explorer {
    use std::{fs, path::Path};

    use ::kithara::ui::render::{Published, ReadValue, Reads, UiEvent, Walk, WriteValue};
    use kithara_test_fixtures::assets;
    use kithara_test_utils::kithara;
    use tempfile::TempDir;

    use super::startup::{listed, tree, with_rig};
    use crate::{
        catalog::canonical_source,
        gui::{message::Message, reads::ReadRoot, rig::Rig, test_fixture},
    };

    const ID: &str = "explorer";
    const STATUS: &str = "source.status@source=explorer";

    fn words(rig: &Rig, key: &str) -> String {
        rig.ui
            .ui
            .package
            .text()
            .get(key)
            .unwrap_or_else(|| panic!("the app words `{key}`"))
            .to_owned()
    }

    fn row_of(rig: &Rig, label: &str) -> usize {
        tree(rig)
            .iter()
            .position(|(_, shown)| shown == label)
            .unwrap_or_else(|| panic!("the tree draws `{label}`: {:?}", tree(rig)))
    }

    fn write(rig: &mut Rig, key: &str, label: &str) {
        let row = row_of(rig, label);
        rig.message(Message::Ui(Published::Host(UiEvent::Write {
            key: key.to_owned(),
            value: WriteValue::Index(row),
        })));
    }

    fn name(folder: &Path) -> String {
        folder
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a temp folder has a UTF-8 name")
            .to_owned()
    }

    fn pick(rig: &mut Rig, folder: &Path) {
        rig.ui.picker.picked(Some(folder.to_path_buf()));
        rig.frame();
        write(rig, "library.toggle", "Music Folders");
    }

    fn show(rig: &mut Rig, folder: &Path) {
        pick(rig, folder);
        write(rig, "library.select", &name(folder));
    }

    fn settled(rig: &mut Rig) {
        let loading = words(rig, "library.status.loading");
        rig.until("the listing lands", Rig::DEADLINE, Rig::frame, |rig| {
            rig.text(STATUS).is_none_or(|status| status != loading)
        });
    }

    fn drawn(rig: &Rig, label: &str) -> (Option<bool>, Option<u32>) {
        let root = ReadRoot::new(&rig.ui);
        let reads = Walk::new(&root);
        let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
            panic!("the library draws its tree");
        };
        let row = rows
            .iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("the tree draws `{label}`"));
        (row.expanded, row.count)
    }

    fn crate_with(files: &[&str]) -> TempDir {
        let dir = TempDir::new().expect("a temp folder");
        for file in files {
            fs::write(dir.path().join(file), b"").expect("the temp folder takes a file");
        }
        dir
    }

    #[kithara::test(native, flash(false))]
    fn a_picked_folder_appears_under_music_folders() {
        let mut rig = Rig::offline();
        let dir = crate_with(&[]);

        pick(&mut rig, dir.path());

        let at = row_of(&rig, "Music Folders");
        assert_eq!(tree(&rig).get(at + 1), Some(&(2, name(dir.path()))));
    }

    #[kithara::test(native, flash(false))]
    fn a_folder_lists_only_the_files_the_app_can_play() {
        let mut rig = Rig::offline();
        let dir = crate_with(&["notes.txt", "track.mp3"]);

        show(&mut rig, dir.path());
        settled(&mut rig);

        let rows = listed(&rig, ID);
        let track = dir.path().join("track.mp3");
        assert_eq!(
            rows.iter()
                .map(|row| (row.title.as_deref(), row.drag.as_deref()))
                .collect::<Vec<_>>(),
            [(Some("track"), track.to_str())],
        );
        assert_eq!(rig.text(STATUS).as_deref(), Some(""));
    }

    #[kithara::test(native, flash(false))]
    fn a_listed_folder_draws_no_count_and_music_folders_counts_its_folders() {
        let mut rig = Rig::offline();
        let dir = crate_with(&["track.mp3"]);

        show(&mut rig, dir.path());
        settled(&mut rig);

        assert_eq!(drawn(&rig, &name(dir.path())).1, None);
        assert_eq!(drawn(&rig, "Music Folders").1, Some(1));
    }

    #[kithara::test(native, flash(false))]
    fn listing_runs_off_the_ui_thread_and_lands_on_a_later_tick() {
        let mut rig = Rig::offline();
        let dir = crate_with(&["track.mp3"]);

        show(&mut rig, dir.path());

        assert!(
            listed(&rig, ID).is_empty(),
            "the select returned before the rows"
        );
        assert_eq!(
            rig.text(STATUS),
            Some(words(&rig, "library.status.loading"))
        );
        rig.until(
            "the rows land on a tick",
            Rig::DEADLINE,
            Rig::frame,
            |rig| !listed(rig, ID).is_empty(),
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_folder_without_playable_files_shows_the_empty_state() {
        let mut rig = Rig::offline();
        let dir = crate_with(&["notes.txt"]);

        show(&mut rig, dir.path());
        settled(&mut rig);

        assert_eq!(rig.text(STATUS), Some(words(&rig, "library.status.empty")));
    }

    #[kithara::test(native, flash(false))]
    fn an_unreadable_folder_shows_the_error_state() {
        let mut rig = Rig::offline();
        let dir = crate_with(&[]);
        let gone = dir.path().join("gone");

        show(&mut rig, &gone);
        settled(&mut rig);

        assert!(listed(&rig, ID).is_empty());
        assert_eq!(rig.text(STATUS), Some(words(&rig, "library.status.error")));
    }

    #[kithara::test(native, flash(false))]
    fn a_node_that_lists_no_rows_shows_the_empty_state() {
        let mut rig = Rig::offline();

        write(&mut rig, "library.select", "Music Folders");

        assert!(listed(&rig, ID).is_empty());
        assert_eq!(rig.text(STATUS), Some(words(&rig, "library.status.empty")));
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn an_explorer_row_dropped_on_deck_a_loads_there_and_becomes_current() {
        with_rig(|rig| {
            let dir = crate_with(&[]);
            fs::write(
                dir.path().join("track.mp3"),
                assets::sine_mp3_a440_2s().bytes(),
            )
            .expect("the temp folder takes a track");
            show(rig, dir.path());
            settled(rig);
            let drag = listed(rig, ID)
                .into_iter()
                .find_map(|row| row.drag)
                .expect("the folder lists its file");

            rig.message(Message::Ui(Published::Host(UiEvent::Write {
                key: "deck.queue.load@deck=a".to_owned(),
                value: WriteValue::Record(test_fixture::dragged(&drag)),
            })));
            rig.pump();

            let source = Some(canonical_source(&drag));
            let queued: Vec<Option<String>> = rig.queues[0]
                .tracks()
                .into_iter()
                .map(|track| track.url)
                .collect();
            assert_eq!(queued, std::slice::from_ref(&source));
            rig.until(
                "the dropped track becomes deck A's current one",
                Rig::DEADLINE,
                |rig| {
                    rig.pump();
                },
                |rig| rig.queues[0].current().and_then(|track| track.url) == source,
            );
            assert!(
                rig.queues[1].tracks().is_empty(),
                "deck B was not the target"
            );
        })
        .await;
    }

    #[kithara::test(native, flash(false))]
    fn home_shows_a_chevron_before_anything_lists_it() {
        let rig = Rig::offline();

        assert_eq!(drawn(&rig, "Home").0, Some(false));
    }

    #[kithara::test(native, flash(false))]
    fn expanding_a_folder_never_selected_lists_its_subfolders() {
        let mut rig = Rig::offline();
        let dir = crate_with(&[]);
        fs::create_dir(dir.path().join("inner")).expect("the temp folder takes a subfolder");
        pick(&mut rig, dir.path());
        let folder = name(dir.path());
        assert_eq!(
            drawn(&rig, &folder).0,
            Some(false),
            "a folder nothing has listed yet may hold subfolders"
        );

        write(&mut rig, "library.toggle", &folder);

        let at = row_of(&rig, &folder);
        rig.until(
            "the subfolders land on a tick",
            Rig::DEADLINE,
            Rig::frame,
            |rig| tree(rig).get(at + 1) == Some(&(3, "inner".to_owned())),
        );
        assert_eq!(drawn(&rig, &folder).0, Some(true));
        assert_eq!(
            drawn(&rig, "inner").0,
            Some(false),
            "a subfolder nothing has listed yet may hold more"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_pressed_row_is_the_one_selected_row_of_its_page() {
        let mut rig = Rig::offline();
        let dir = crate_with(&["b.mp3", "c.mp3"]);
        let folder = name(dir.path());
        show(&mut rig, dir.path());
        settled(&mut rig);
        let selected = |rig: &Rig| -> Vec<bool> {
            listed(rig, ID)
                .into_iter()
                .map(|row| row.selected)
                .collect()
        };

        for row in [0, 1] {
            rig.message(Message::Ui(Published::Host(UiEvent::Write {
                key: format!("source.select@source={ID}"),
                value: WriteValue::Index(row),
            })));
        }
        assert_eq!(selected(&rig), [false, true]);

        fs::write(dir.path().join("a.mp3"), b"").expect("the temp folder takes a file");
        write(&mut rig, "library.select", &folder);
        rig.until("the relisting lands", Rig::DEADLINE, Rig::frame, |rig| {
            listed(rig, ID).len() == 3
        });
        assert_eq!(selected(&rig), [false, false, true], "c.mp3 stays selected");
    }

    #[kithara::test(native, flash(false))]
    fn reselecting_a_listed_folder_keeps_it_until_the_newer_listing_lands() {
        let mut rig = Rig::offline();
        let dir = crate_with(&["a.mp3"]);
        fs::create_dir(dir.path().join("inner")).expect("the temp folder takes a subfolder");
        let folder = name(dir.path());
        show(&mut rig, dir.path());
        settled(&mut rig);
        fs::write(dir.path().join("b.mp3"), b"").expect("the temp folder takes a file");
        write(&mut rig, "library.toggle", &folder);

        write(&mut rig, "library.select", "Music Folders");
        write(&mut rig, "library.select", &folder);

        assert!(
            !listed(&rig, ID).is_empty(),
            "the rows stay while the folder is listed again"
        );
        let at = row_of(&rig, &folder);
        assert_eq!(tree(&rig).get(at + 1), Some(&(3, "inner".to_owned())));
        rig.until(
            "the newer listing replaces the rows",
            Rig::DEADLINE,
            Rig::frame,
            |rig| listed(rig, ID).len() == 2,
        );
    }
}

#[kithara::test]
fn library_followup_source_roots_start_expanded() {
    let (probe, _) = Probe::registered("probe.label");
    let library =
        Library::new(vec![StartupSource::registered(Vec::new()), probe], &words()).unwrap();
    let roots = library
        .tree()
        .into_iter()
        .filter(|row| row.depth == 0)
        .map(|row| row.expanded)
        .collect::<Vec<_>>();
    assert_eq!(roots, [Some(true), Some(true)]);
    assert_eq!(selected(&library.tree()), ["Startup"]);
}

#[cfg(unix)]
#[kithara::test]
fn folder_listing_skips_broken_media_links_and_follows_directory_links() {
    let dir = kithara_test_utils::temp_dir();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let linked = dir.path().join("linked");
    std::os::unix::fs::symlink(&target, &linked).unwrap();
    std::os::unix::fs::symlink(dir.path().join("missing"), dir.path().join("ghost.mp3")).unwrap();
    let super::listing::Listing::Listed(folder) = super::listing::list(dir.path()) else {
        panic!("fixture is readable");
    };
    assert!(folder.folders.contains(&linked));
    assert!(folder.tracks.is_empty());
}
