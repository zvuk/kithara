use std::{
    fmt::Display,
    fs::create_dir_all,
    iter::once,
    path::{Path, PathBuf},
};

use super::{Film, Stage, write_geometry, write_png};

/// Photographs a film through one stage into a directory, recording beside it
/// the geometry the set was taken at, and answering with the files written.
///
/// # Errors
/// Fails when the directory cannot be made, when the stage cannot open or
/// draw a page, or when a photograph cannot be encoded. A film of no pages is
/// refused rather than reported as a set of nothing: a capture that writes no
/// file and exits successfully reads as a comparison that found no difference.
pub fn shoot_set<S>(stage: &mut S, film: &Film<S::Page>, dir: &Path) -> Result<Vec<PathBuf>, String>
where
    S: Stage,
    S::Page: Display,
{
    if film.pages.is_empty() {
        return Err("a film of no pages photographs nothing".to_owned());
    }
    create_dir_all(dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let frame = stage.geometry();
    write_geometry(dir, frame)?;
    let mut written: Vec<PathBuf> = Vec::new();
    for page in &film.pages {
        stage.turn(page)?;
        for photo in 0..film.photos {
            if photo > 0 {
                for _ in 0..film.steps {
                    stage.tick();
                }
            }
            let path = dir.join(film.file(page, photo));
            write_png(&path, frame.width, frame.height, once(stage.shoot()?))?;
            written.push(path);
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::{Film, Stage, shoot_set};
    use crate::capture::{Geometry, read_geometry};

    struct Cards {
        frame: Geometry,
        page: &'static str,
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
}
