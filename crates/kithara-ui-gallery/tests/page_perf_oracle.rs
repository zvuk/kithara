use kithara_test_utils::kithara;

#[path = "page_perf/pages.rs"]
mod pages;
use kithara_ui_gallery::{demo, fixture, sections};
use pages::{Fixture, Program, consts};

fn use_program(program: Program) {
    match program {
        Program::Buckets(counts) => {
            let _ = counts;
        }
        Program::Wheels(counts) | Program::Drag(counts) => {
            let _ = counts;
        }
        Program::Idle | Program::Tick => {}
    }
}

#[kithara::test]
fn measured_pages_are_gallery_pages_or_harness_owned() {
    let _ = consts::IMMEDIATE_FORMAT;
    let fixture = Fixture::default();
    let _ = fixture.config();
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
    println!("unmeasured gallery pages: {unmeasured:?}");
    for page in consts::PAGES {
        let _ = fixture.compiled(page);
        use_program(page.program);
        let _ = (page.frames, page.moving, page.pointer_at, page.fenced);
        assert!(
            page.own.is_some() || tabs.contains(&page.tab),
            "page is not a gallery page or harness-owned: {}",
            page.name
        );
    }
}
