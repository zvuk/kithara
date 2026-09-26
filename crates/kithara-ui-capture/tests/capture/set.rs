use kithara_test_utils::kithara;
use kithara_ui_capture::{Film, Geometry, Stage, read_geometry, shoot_set};
use tempfile::tempdir;

struct Cards {
    page: &'static str,
    frame: Geometry,
    pixels: Vec<u8>,
    ticks: usize,
}

impl Stage for Cards {
    type Page = &'static str;

    fn geometry(&self) -> Geometry {
        self.frame
    }

    fn shoot(&mut self) -> Result<&[u8], String> {
        Ok(&self.pixels)
    }

    fn tick(&mut self) {
        self.ticks += 1;
    }

    fn turn(&mut self, page: &Self::Page) -> Result<(), String> {
        self.page = page;
        Ok(())
    }
}

fn cards() -> Cards {
    Cards {
        frame: Geometry {
            height: 1,
            scale: 2.0,
            width: 1,
        },
        page: "",
        pixels: vec![0, 0, 0, 255],
        ticks: 0,
    }
}

#[kithara::test]
fn a_film_writes_every_photo_and_advances_between_them() {
    let dir = tempdir().expect("a private capture directory");
    let film = Film::new(vec!["clock", "wave"], 2, 3).expect("a moving film");
    let mut stage = cards();

    let written = shoot_set(&mut stage, &film, dir.path()).expect("a complete capture set");

    assert_eq!(written.len(), 4);
    assert!(written.iter().all(|path| path.exists()));
    assert_eq!(stage.page, "wave");
    assert_eq!(stage.ticks, 6);
    assert_eq!(read_geometry(dir.path()), Some(stage.frame));
}

#[kithara::test]
fn a_film_without_pages_is_refused() {
    let dir = tempdir().expect("a private capture directory");
    let film = Film::stills(Vec::<&str>::new());

    assert!(shoot_set(&mut cards(), &film, dir.path()).is_err());
    assert!(!dir.path().join("frame.txt").exists());
}
