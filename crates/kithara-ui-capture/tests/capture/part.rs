use std::{fs::File, path::Path};

use kithara_test_utils::kithara;
use kithara_ui_capture::{Geometry, Locate, Region, Stage, part_file, shoot_part};
use kurbo::Rect;
use png::Decoder;
use tempfile::tempdir;

/// A stage that photographs the pixels it was built with, and knows where
/// one control of them is.
struct Card {
    frame: Geometry,
    rect: Option<Rect>,
    pixels: Vec<u8>,
}

impl Stage for Card {
    type Page = &'static str;

    fn geometry(&self) -> Geometry {
        self.frame
    }

    fn shoot(&mut self) -> Result<&[u8], String> {
        Ok(&self.pixels)
    }

    fn tick(&mut self) {}

    fn turn(&mut self, _page: &Self::Page) -> Result<(), String> {
        Ok(())
    }
}

impl Locate for Card {
    fn locate(&self, _path: &str) -> Option<Rect> {
        self.rect
    }
}

/// A frame whose every pixel says which one it is, so a picture cut out of
/// it can be read back as the place it was cut from.
fn card(rect: Rect) -> Card {
    Card {
        frame: Geometry {
            height: 4,
            scale: 1.0,
            width: 4,
        },
        pixels: (0..4u8)
            .flat_map(|y| (0..4u8).flat_map(move |x| [x, y, 0, 255]))
            .collect(),
        rect: Some(rect),
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect::new(x, y, x + w, y + h)
}

/// The size and RGBA8 pixels of a picture that was written.
fn read(path: &Path) -> ((u32, u32), Vec<u8>) {
    let file = File::open(path).expect("a picture that was just written");
    let mut reader = Decoder::new(file).read_info().expect("a PNG header");
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).expect("one PNG frame");
    rgba.truncate(info.buffer_size());
    ((info.width, info.height), rgba)
}

#[kithara::test]
fn a_photograph_of_a_control_carries_the_pixels_it_was_laid_out_over() {
    let dir = tempdir().expect("a private capture directory");
    let file = shoot_part(
        &mut card(rect(1.0, 2.0, 2.0, 1.0)),
        &"clock",
        "deck/play",
        dir.path(),
    )
    .expect("a control the page draws");
    assert_eq!(read(&file), ((2, 1), vec![1, 2, 0, 255, 2, 2, 0, 255]));
}

#[kithara::test]
fn a_photograph_of_a_control_lands_under_its_own_name() {
    let dir = tempdir().expect("a private capture directory");
    let file = shoot_part(
        &mut card(rect(1.0, 1.0, 2.0, 2.0)),
        &"clock",
        "deck/play",
        dir.path(),
    )
    .expect("a control the page draws");
    assert_eq!(file, dir.path().join("clock-deck-play.png"));
}

#[kithara::test]
fn a_page_that_draws_no_such_control_is_refused() {
    let dir = tempdir().expect("a private capture directory");
    let mut stage = card(rect(1.0, 1.0, 2.0, 2.0));
    stage.rect = None;
    assert!(shoot_part(&mut stage, &"clock", "deck/play", dir.path()).is_err());
}

#[kithara::test]
fn a_control_that_leaves_the_frame_is_refused() {
    let dir = tempdir().expect("a private capture directory");
    let mut stage = card(rect(2.0, 0.0, 3.0, 1.0));
    assert!(shoot_part(&mut stage, &"clock", "deck/play", dir.path()).is_err());
}

#[kithara::test]
fn a_control_too_small_for_the_scale_it_is_photographed_at_is_refused() {
    let dir = tempdir().expect("a private capture directory");
    let mut stage = card(rect(0.0, 0.0, 1.0, 1.0));
    stage.frame.scale = 0.1;
    assert!(shoot_part(&mut stage, &"clock", "deck/play", dir.path()).is_err());
}

#[kithara::test]
fn a_frame_the_photograph_does_not_fill_is_refused() {
    let dir = tempdir().expect("a private capture directory");
    let mut stage = card(rect(0.0, 0.0, 2.0, 2.0));
    stage.pixels.truncate(8);
    assert!(shoot_part(&mut stage, &"clock", "deck/play", dir.path()).is_err());
}

#[kithara::test]
fn a_control_covers_as_many_pixels_as_the_scale_it_is_drawn_at() {
    assert_eq!(
        Region::of(rect(3.0, 1.0, 10.0, 5.0), 2.0).expect("a control inside the frame"),
        Region {
            height: 10,
            width: 20,
            x: 6,
            y: 2,
        }
    );
}

#[kithara::test]
fn a_control_laid_out_before_the_frame_begins_is_refused() {
    assert!(Region::of(rect(-1.0, 1.0, 10.0, 5.0), 1.0).is_err());
}

#[kithara::test]
fn a_control_of_no_size_is_refused() {
    assert!(Region::of(rect(1.0, 1.0, 10.0, 0.0), 1.0).is_err());
}

#[kithara::test]
fn a_photograph_of_a_control_is_named_after_the_page_and_the_control() {
    assert_eq!(
        part_file(&"transport", "gallery/transport/play"),
        "transport-gallery-transport-play.png"
    );
}
