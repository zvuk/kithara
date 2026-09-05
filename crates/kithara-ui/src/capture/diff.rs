use std::{
    collections::HashMap,
    fmt,
    fs::{File, create_dir_all, read_dir, read_to_string},
    iter::once,
    path::{Path, PathBuf},
};

use num_traits::cast::AsPrimitive;

use super::geometry::{read_geometry, write_png};

/// A channel difference below this is rasteriser noise: two hosts run
/// different rasterisers, so antialiased edges never match bit for bit.
const NOISE: u8 = 24;

/// How far apart one page's two photographs are, as shares of its pixels.
#[derive(Clone, Copy, Debug)]
struct Shares {
    differing: f64,
    ink: f64,
}

impl Shares {
    /// The number a budget is judged by: whichever of the two is worse.
    fn worst(self) -> f64 {
        self.differing.max(self.ink)
    }
}

/// One page of the comparison. A page missing from the second set has no
/// shares, which no budget forgives.
struct Row {
    shares: Option<Shares>,
    page: String,
}

/// A page that differed by more than it was allowed to.
struct Over {
    page: String,
    allowed: f64,
    worst: f64,
}

/// What comparing two capture sets came out as: the pages, where the
/// difference masks were written, and — when a budget was given — the pages
/// that went over it.
pub struct Report {
    masks: PathBuf,
    over: Vec<Over>,
    rows: Vec<Row>,
    judged: bool,
    pages: usize,
}

impl Report {
    /// Whether every page stayed within its budget. A run given no budget
    /// decided nothing and passes.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.over.is_empty()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "page                          differing pixels     ink disagreement"
        )?;
        for row in &self.rows {
            match row.shares {
                Some(shares) => writeln!(
                    formatter,
                    "{:<30}{:>15.1}%{:>20.1}%",
                    row.page,
                    shares.differing * 100.0,
                    shares.ink * 100.0
                )?,
                None => writeln!(formatter, "{:<30}{:>16}", row.page, "missing")?,
            }
        }
        writeln!(
            formatter,
            "\ndifference masks in {}; a channel gap under {NOISE} is treated as rasteriser \
             noise, because the two hosts rasterise with different engines. ink disagreement is \
             the share of pixels drawn by one host and left as background by the other.",
            self.masks.display()
        )?;
        if !self.judged {
            return writeln!(
                formatter,
                "{} page(s) compared; no budget given, so nothing was decided",
                self.pages
            );
        }
        for over in &self.over {
            if over.worst.is_finite() {
                writeln!(
                    formatter,
                    "over budget: {} differs by {:.1}%, allowed {:.1}%",
                    over.page, over.worst, over.allowed
                )?;
            } else {
                writeln!(
                    formatter,
                    "over budget: {} is missing from one of the sets",
                    over.page
                )?;
            }
        }
        writeln!(
            formatter,
            "{} page(s) compared, {} over budget",
            self.pages,
            self.over.len()
        )
    }
}

/// Compares two capture sets page by page, writing a difference mask per page
/// into `masks`.
///
/// It is only meaningful when both sets were photographed at the same
/// geometry: each set records its own in `frame.txt`, and a mismatch is
/// refused, because two hosts scaled differently can be made to agree or
/// disagree at will.
///
/// A budget turns the numbers into a gate: the file names a per-page
/// allowance, and a page over its allowance — or missing from either set —
/// fails the run. Without one this only reports.
///
/// # Errors
/// Fails when a set has no recorded geometry, the two disagree on it, a page
/// cannot be read, or a mask cannot be written.
pub fn compare(
    left: &Path,
    right: &Path,
    masks: &Path,
    budget: Option<&Path>,
) -> Result<Report, String> {
    match (read_geometry(left), read_geometry(right)) {
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
    create_dir_all(masks).map_err(|error| format!("create {}: {error}", masks.display()))?;
    let budget = budget.map(read_budget).transpose()?;

    let mut pages = 0;
    let mut rows: Vec<Row> = Vec::new();
    for entry in read_dir(left).map_err(|error| format!("read {left:?}: {error}"))? {
        let path = entry
            .map_err(|error| format!("read {left:?}: {error}"))?
            .path();
        if path.extension().is_none_or(|extension| extension != "png") {
            continue;
        }
        let Some(page) = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            continue;
        };
        let twin = right.join(&page);
        if !twin.exists() {
            rows.push(Row { page, shares: None });
            continue;
        }
        let a = read_png(&path)?;
        let b = read_png(&twin)?;
        if a.width != b.width || a.height != b.height {
            return Err(format!(
                "{page}: {}x{} against {}x{}",
                a.width, a.height, b.width, b.height
            ));
        }
        let (differing, mask) = difference(&a, &b);
        let ink = ink_disagreement(&a, &b);
        write_png(&masks.join(&page), a.width, a.height, once(mask.as_slice()))?;
        rows.push(Row {
            page,
            shares: Some(Shares { differing, ink }),
        });
        pages += 1;
    }

    rows.sort_by(|left, right| {
        right
            .shares
            .map(Shares::worst)
            .partial_cmp(&left.shares.map(Shares::worst))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let over = budget
        .as_ref()
        .map(|budget| over_budget(&rows, budget))
        .unwrap_or_default();
    Ok(Report {
        over,
        pages,
        rows,
        judged: budget.is_some(),
        masks: masks.to_owned(),
    })
}

/// The pages that differed by more than they were allowed to.
fn over_budget(rows: &[Row], budget: &[(String, f64)]) -> Vec<Over> {
    rows.iter()
        .filter_map(|row| {
            let allowed = budget
                .iter()
                .find(|(page, _)| *page == row.page)
                .map_or(0.0, |(_, allowed)| *allowed);
            let worst = row
                .shares
                .map_or(f64::INFINITY, |shares| shares.worst() * 100.0);
            (worst > allowed).then(|| Over {
                allowed,
                worst,
                page: row.page.clone(),
            })
        })
        .collect()
}

/// The share of pixels each page is allowed to differ by, as whole percent.
///
/// One line per page: the file name, then the allowance. A page with no line
/// is allowed nothing, so a set that grows a page has to say what that page is
/// worth before the gate will pass.
fn read_budget(path: &Path) -> Result<Vec<(String, f64)>, String> {
    let text = read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (page, share) = line
                .split_once(char::is_whitespace)
                .ok_or_else(|| format!("{}: expected `<page.png> <percent>`", path.display()))?;
            let share = share
                .trim()
                .parse::<f64>()
                .map_err(|error| format!("{}: {line:?}: {error}", path.display()))?;
            Ok((page.to_owned(), share))
        })
        .collect()
}

pub(super) struct Image {
    pub(super) rgba: Vec<u8>,
    pub(super) height: u32,
    pub(super) width: u32,
}

pub(super) fn read_png(path: &Path) -> Result<Image, String> {
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
        rgba,
        height: info.height,
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
/// floor [`difference`] already uses, applied here between a host and its own
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
/// one of the two sets.
///
/// A missing control is thin ink on a wide page, so it barely moves
/// [`difference`]'s pixel count; judging each host against its own background
/// instead of against the other host's pixels catches the absence directly.
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
        env, fs,
        iter::once,
        process,
        sync::atomic::{AtomicU64, Ordering},
    };

    use kithara_test_utils::kithara;

    use super::{Image, PathBuf, compare, create_dir_all, difference, ink_disagreement, write_png};
    use crate::capture::{Geometry, write_geometry};

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
            "kithara-capture-diff-{name}-{}-{id}",
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
        let masks = root.join("out");
        create_dir_all(&left_dir).unwrap();
        create_dir_all(&right_dir).unwrap();

        let frame = Geometry {
            height: 1,
            scale: 1.0,
            width: 10,
        };
        write_geometry(&left_dir, frame).unwrap();
        write_geometry(&right_dir, frame).unwrap();

        // Same pixels as ink_present_on_one_side_only: diff share is 0%, ink
        // share is 10%. A budget of 5% sits strictly between the two, so the
        // gate only fails if it is judged by the larger number.
        let mut left = solid(10, 1, [200, 200, 200, 255]);
        left.rgba[0..4].copy_from_slice(&[226, 226, 226, 255]);
        let right = solid(10, 1, [205, 205, 205, 255]);
        write_png(
            &left_dir.join("page.png"),
            10,
            1,
            once(left.rgba.as_slice()),
        )
        .unwrap();
        write_png(
            &right_dir.join("page.png"),
            10,
            1,
            once(right.rgba.as_slice()),
        )
        .unwrap();

        let budget_path = root.join("budget.txt");
        fs::write(&budget_path, "page.png 5\n").unwrap();

        let report = compare(&left_dir, &right_dir, &masks, Some(&budget_path))
            .expect("two complete capture sets");
        let passed = report.passed();
        let message = report.to_string();

        fs::remove_dir_all(&root).ok();

        assert!(!passed);
        assert!(message.contains("over budget: page.png differs by 10.0%, allowed 5.0%"));
        assert!(message.contains("1 page(s) compared, 1 over budget"));
    }

    #[kithara::test]
    fn an_unjudged_comparison_reports_a_missing_page_without_failing() {
        let root = scratch_dir("missing");
        let left_dir = root.join("left");
        let right_dir = root.join("right");
        let masks = root.join("out");
        create_dir_all(&left_dir).unwrap();
        create_dir_all(&right_dir).unwrap();

        let frame = Geometry {
            height: 1,
            scale: 1.0,
            width: 1,
        };
        write_geometry(&left_dir, frame).unwrap();
        write_geometry(&right_dir, frame).unwrap();
        write_png(
            &left_dir.join("missing.png"),
            1,
            1,
            once([0, 0, 0, 255].as_slice()),
        )
        .unwrap();

        let report = compare(&left_dir, &right_dir, &masks, None)
            .expect("matching geometry is enough for an unjudged comparison");
        let message = report.to_string();

        fs::remove_dir_all(&root).ok();

        assert!(report.passed());
        assert!(message.contains("missing.png"));
        assert!(message.contains("missing"));
        assert!(message.contains("0 page(s) compared; no budget given, so nothing was decided"));
    }
}
