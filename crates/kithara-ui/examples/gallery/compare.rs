//! Compares two capture sets page by page and says, in numbers, where they
//! disagree.
//!
//! Enabled by `KITHARA_GALLERY_COMPARE=<a>:<b>:<out>`. It is only meaningful
//! when both sets were photographed at the same geometry — the capture writes
//! `frame.txt` beside the pages and this refuses to compare across a mismatch,
//! because two hosts scaled differently can be made to agree or disagree at
//! will.

use std::{
    collections::HashMap,
    env,
    fs::{File, create_dir_all},
    path::{Path, PathBuf},
};

use num_traits::cast::AsPrimitive;

use super::capture::{read_frame, write_png};

/// A channel difference below this is rasteriser noise: the two hosts run
/// different rasterisers, so antialiased edges never match bit for bit.
const NOISE: u8 = 24;

/// What a comparison run decided.
pub(super) enum Verdict {
    /// No comparison was asked for; the caller falls through.
    NotAsked,
    /// Every page compared within its budget, or no budget was given.
    Passed,
    /// A page differed more than its budget allows, or was missing from a set.
    Failed,
}

/// Runs the comparison when asked.
///
/// A budget turns the numbers into a gate: `KITHARA_GALLERY_COMPARE_BUDGET`
/// names a file of per-page allowances, and a page over its allowance — or
/// missing from either set — fails the run. Without one this only reports, the
/// way it always has.
pub(super) fn run() -> Verdict {
    let Some(spec) = env::var_os("KITHARA_GALLERY_COMPARE") else {
        return Verdict::NotAsked;
    };
    let budget = env::var_os("KITHARA_GALLERY_COMPARE_BUDGET").map(PathBuf::from);
    match compare(&spec.to_string_lossy(), budget.as_deref()) {
        Ok(true) => Verdict::Passed,
        Ok(false) => Verdict::Failed,
        Err(error) => {
            eprintln!("compare failed: {error}");
            Verdict::Failed
        }
    }
}

/// The share of pixels each page is allowed to differ by, as whole percent.
///
/// One line per page: the file name, then the allowance. A page with no line
/// is allowed nothing, so a set that grows a page has to say what that page is
/// worth before the gate will pass.
fn read_budget(path: &Path) -> Result<Vec<(String, f64)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, share) = line
                .split_once(char::is_whitespace)
                .ok_or_else(|| format!("{}: expected `<page.png> <percent>`", path.display()))?;
            let share = share
                .trim()
                .parse::<f64>()
                .map_err(|error| format!("{}: {line:?}: {error}", path.display()))?;
            Ok((name.to_owned(), share))
        })
        .collect()
}

fn compare(spec: &str, budget: Option<&Path>) -> Result<bool, String> {
    let parts = spec.split(':').collect::<Vec<_>>();
    let [left, right, out] = parts.as_slice() else {
        return Err("expected KITHARA_GALLERY_COMPARE=<a>:<b>:<out>".to_owned());
    };
    let (left, right, out) = (Path::new(left), Path::new(right), PathBuf::from(out));
    match (read_frame(left), read_frame(right)) {
        (Some(a), Some(b)) if a != b => {
            return Err(format!(
                "the two sets were photographed differently — {}x{} at {}x versus {}x{} at {}x; \
                 recapture the second into the first's directory so it inherits the geometry",
                a.width, a.height, a.scale, b.width, b.height, b.scale
            ));
        }
        (None, _) | (_, None) => {
            return Err("a capture set has no frame.txt, so its geometry is unknown".to_owned());
        }
        _ => {}
    }
    create_dir_all(&out).map_err(|error| format!("create {}: {error}", out.display()))?;
    let budget = budget.map(read_budget).transpose()?;

    let mut pages = 0;
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(left).map_err(|error| format!("read {left:?}: {error}"))? {
        let path = entry
            .map_err(|error| format!("read {left:?}: {error}"))?
            .path();
        if path.extension().is_none_or(|ext| ext != "png") {
            continue;
        }
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            continue;
        };
        let twin = right.join(&name);
        if !twin.exists() {
            rows.push((name, None));
            continue;
        }
        let a = read_png(&path)?;
        let b = read_png(&twin)?;
        if a.width != b.width || a.height != b.height {
            return Err(format!(
                "{name}: {}x{} against {}x{}",
                a.width, a.height, b.width, b.height
            ));
        }
        let (share, mask) = difference(&a, &b);
        let ink = ink_disagreement(&a, &b);
        write_png(&out.join(&name), &mask, a.width, a.height)?;
        rows.push((name, Some((share, ink))));
        pages += 1;
    }

    rows.sort_by(|left, right| {
        let worst = |shares: Option<(f64, f64)>| shares.map(|(share, ink)| share.max(ink));
        worst(right.1)
            .partial_cmp(&worst(left.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    println!("page                          differing pixels     ink disagreement");
    for (name, shares) in &rows {
        match shares {
            Some((share, ink)) => {
                println!("{name:<30}{:>15.1}%{:>20.1}%", share * 100.0, ink * 100.0);
            }
            None => println!("{name:<30}{:>16}", "missing"),
        }
    }
    println!(
        "\ndifference masks in {}; a channel gap under {NOISE} is treated as rasteriser noise, \
         because the two hosts rasterise with different engines. ink disagreement is the share \
         of pixels drawn by one host and left as background by the other.",
        out.display()
    );
    let Some(budget) = budget else {
        println!("{pages} page(s) compared; no budget given, so nothing was decided");
        return Ok(true);
    };
    let over = rows
        .iter()
        .filter_map(|(name, shares)| {
            let allowed = budget
                .iter()
                .find(|(page, _)| page == name)
                .map_or(0.0, |(_, allowed)| *allowed);
            match shares {
                Some((share, ink)) => {
                    let worst = share.max(*ink) * 100.0;
                    (worst > allowed).then_some((name, worst, allowed))
                }
                None => Some((name, f64::INFINITY, allowed)),
            }
        })
        .collect::<Vec<_>>();
    for (name, worst, allowed) in &over {
        if worst.is_finite() {
            println!("over budget: {name} differs by {worst:.1}%, allowed {allowed:.1}%");
        } else {
            println!("over budget: {name} is missing from one of the sets");
        }
    }
    println!("{pages} page(s) compared, {} over budget", over.len());
    Ok(over.is_empty())
}

struct Image {
    height: u32,
    rgba: Vec<u8>,
    width: u32,
}

fn read_png(path: &Path) -> Result<Image, String> {
    let file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let decoder = png::Decoder::new(file);
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("decode {}: {error}", path.display()))?;
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut rgba)
        .map_err(|error| format!("decode {}: {error}", path.display()))?;
    rgba.truncate(info.buffer_size());
    Ok(Image {
        height: info.height,
        rgba,
        width: info.width,
    })
}

/// The share of pixels that differ beyond rasteriser noise, and a mask that
/// paints those pixels so the eye can find them.
fn difference(left: &Image, right: &Image) -> (f64, Vec<u8>) {
    let mut mask = vec![0; left.rgba.len()];
    let mut differing = 0_usize;
    let total = left.rgba.len() / 4;
    for (index, (a, b)) in left
        .rgba
        .chunks_exact(4)
        .zip(right.rgba.chunks_exact(4))
        .enumerate()
    {
        let gap = a
            .iter()
            .zip(b)
            .take(3)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let at = index * 4;
        if gap > NOISE {
            differing += 1;
            mask[at] = 255;
            mask[at + 1] = 32;
            mask[at + 2] = 96;
            mask[at + 3] = 255;
        } else {
            let grey = u8::try_from(u16::from(a[0]) / 6).unwrap_or(0);
            mask[at] = grey;
            mask[at + 1] = grey;
            mask[at + 2] = grey;
            mask[at + 3] = 255;
        }
    }
    let share = if total == 0 {
        0.0
    } else {
        AsPrimitive::<f64>::as_(differing) / AsPrimitive::<f64>::as_(total)
    };
    (share, mask)
}

/// The page's background colour is not passed in anywhere the comparison can
/// read; every page clears to one colour and draws only a minority of pixels
/// over it, so the most common colour in a capture stands in for it.
fn background_color(image: &Image) -> [u8; 4] {
    let mut counts: HashMap<[u8; 4], usize> = HashMap::new();
    for pixel in image.rgba.chunks_exact(4) {
        *counts
            .entry([pixel[0], pixel[1], pixel[2], pixel[3]])
            .or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map_or([0, 0, 0, 0], |(color, _)| color)
}

/// A pixel that differs from a background by more than the rasteriser-noise
/// floor `difference` already uses, applied here between a host and its own
/// background instead of between the two hosts.
fn is_ink(pixel: &[u8], background: [u8; 4]) -> bool {
    pixel
        .iter()
        .zip(background)
        .take(3)
        .map(|(a, b)| a.abs_diff(b))
        .max()
        .unwrap_or(0)
        > NOISE
}

/// The share of pixels that are ink — drawn over the background — in exactly
/// one of the two sets. A missing control is thin ink on a wide page, so it
/// barely moves `difference`'s pixel count; judging each host against its own
/// background instead of against the other host's pixels catches the absence
/// directly.
fn ink_disagreement(left: &Image, right: &Image) -> f64 {
    let left_bg = background_color(left);
    let right_bg = background_color(right);
    let total = left.rgba.len() / 4;
    let disagreeing = left
        .rgba
        .chunks_exact(4)
        .zip(right.rgba.chunks_exact(4))
        .filter(|(a, b)| is_ink(a, left_bg) != is_ink(b, right_bg))
        .count();
    if total == 0 {
        0.0
    } else {
        AsPrimitive::<f64>::as_(disagreeing) / AsPrimitive::<f64>::as_(total)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs, process,
        sync::atomic::{AtomicU64, Ordering},
    };

    use kithara_test_utils::kithara;

    use super::*;
    use crate::capture::{Frame, write_frame};

    static DIR_ID: AtomicU64 = AtomicU64::new(0);

    fn solid(width: u32, height: u32, color: [u8; 4]) -> Image {
        Image {
            height,
            rgba: color.repeat((width * height) as usize),
            width,
        }
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let id = DIR_ID.fetch_add(1, Ordering::Relaxed);
        env::temp_dir().join(format!(
            "kithara-gallery-compare-{name}-{}-{id}",
            process::id()
        ))
    }

    #[kithara::test]
    fn identical_images_score_zero_on_both_numbers() {
        let a = solid(4, 4, [200, 200, 200, 255]);
        let b = solid(4, 4, [200, 200, 200, 255]);

        let (share, _) = difference(&a, &b);
        let ink = ink_disagreement(&a, &b);

        assert_eq!(share, 0.0);
        assert_eq!(ink, 0.0);
    }

    #[kithara::test]
    fn ink_present_on_one_side_only_scores_high_even_when_pixel_diff_is_low() {
        // 226 sits within noise of right's 205 background, so `difference` never
        // flags it, but it sits outside noise of left's own 200 background: ink
        // on the left, plain background on the right — a control missing on one host.
        let mut left = solid(10, 1, [200, 200, 200, 255]);
        left.rgba[0..4].copy_from_slice(&[226, 226, 226, 255]);
        let right = solid(10, 1, [205, 205, 205, 255]);

        let (share, _) = difference(&left, &right);
        let ink = ink_disagreement(&left, &right);

        assert_eq!(share, 0.0);
        assert_eq!(ink, 0.1);
    }

    #[kithara::test]
    fn gate_fails_on_the_larger_of_the_two_numbers() {
        let root = scratch_dir("gate");
        let left_dir = root.join("left");
        let right_dir = root.join("right");
        let out_dir = root.join("out");
        create_dir_all(&left_dir).unwrap();
        create_dir_all(&right_dir).unwrap();

        let frame = Frame {
            height: 1,
            scale: 1.0,
            width: 10,
        };
        write_frame(&left_dir, frame).unwrap();
        write_frame(&right_dir, frame).unwrap();

        // Same pixels as ink_present_on_one_side_only: diff share is 0%, ink
        // share is 10%. A budget of 5% sits strictly between the two, so the
        // gate only fails if it is judged by the larger number.
        let mut left = solid(10, 1, [200, 200, 200, 255]);
        left.rgba[0..4].copy_from_slice(&[226, 226, 226, 255]);
        let right = solid(10, 1, [205, 205, 205, 255]);
        write_png(&left_dir.join("page.png"), &left.rgba, 10, 1).unwrap();
        write_png(&right_dir.join("page.png"), &right.rgba, 10, 1).unwrap();

        let budget_path = root.join("budget.txt");
        fs::write(&budget_path, "page.png 5\n").unwrap();

        let spec = format!(
            "{}:{}:{}",
            left_dir.display(),
            right_dir.display(),
            out_dir.display()
        );
        let result = compare(&spec, Some(&budget_path));

        fs::remove_dir_all(&root).ok();

        assert_eq!(result, Ok(false));
    }
}
