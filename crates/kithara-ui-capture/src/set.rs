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
