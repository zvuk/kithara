use kithara_test_utils::kithara;
use kithara_ui::draw::Pt;
use kithara_ui_gallery::{demo, fixture, sections};

#[derive(Clone, Copy)]
enum Program {
    Idle,
    Buckets(&'static [u16]),
    Tick,
    Wheels(&'static [usize]),
    Drag(&'static [usize]),
}

struct Page {
    name: &'static str,
    own: Option<&'static str>,
    tab: &'static str,
    frames: usize,
    program: Program,
    moving: Option<&'static str>,
    pointer_at: Pt,
    fenced: bool,
}

#[path = "page_perf/pages.rs"]
mod pages;

use kithara_ui::view::ViewState;
use pages::consts;

impl Page {
    fn document(&self) -> &'static str {
        self.own.unwrap_or_else(sections::entry)
    }

    fn standing(&self) -> ViewState {
        let mut view = ViewState::default();
        if self.own.is_none() {
            view.stand(sections::PAGE, self.tab);
        }
        view
    }
}

#[kithara::test]
fn measured_pages_are_gallery_pages_or_harness_owned() {
    let tabs = sections::pages();
    let demos = sections::modules();
    let unmeasured: Vec<_> = tabs
        .iter()
        .chain(demos)
        .copied()
        .filter(|tab| {
            !consts::PAGES
                .iter()
                .any(|page| page.own.is_none() && page.tab == *tab)
        })
        .collect();
    assert!(
        unmeasured.is_empty(),
        "unmeasured gallery pages: {unmeasured:?}"
    );
    for page in consts::PAGES {
        let _ = (
            page.frames,
            page.program,
            page.moving,
            page.pointer_at,
            page.fenced,
        );
        assert!(
            page.own.is_some() || tabs.contains(&page.tab),
            "page is not a gallery page or harness-owned: {}",
            page.name
        );
    }
}
