use kurbo::{BezPath, PathEl, Point};
use num_traits::ToPrimitive;
use roxmltree::{Document, Node};

use super::path::{FillRule, Outline, Path, Verb};
use crate::geom::Pt;

/// Why a document could not be read as an outline.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SvgError {
    #[error("the document is not well-formed XML: {0}")]
    Malformed(String),
    #[error("the root element is <{0}>, not <svg>")]
    NotSvg(String),
    #[error("the document has no viewBox, so its art has no size of its own")]
    NoViewBox,
    #[error("the viewBox {0:?} is not four numbers with a positive extent")]
    ViewBox(String),
    #[error("the document draws with <{0}>, which only <path> is read here")]
    NotAPath(String),
    #[error("a <path> has no d")]
    NoData,
    #[error("a <path> asks for {0:?}, which is not a fill rule")]
    Rule(String),
    #[error("two paths disagree about the fill rule, which one outline cannot hold")]
    MixedRules,
    #[error("a path could not be read: {0}")]
    Data(String),
    #[error("a path reaches a coordinate no pixel can hold")]
    Coordinate,
    #[error("the document draws nothing")]
    Empty,
}

/// Reads one SVG document as a single outline in the unit square.
///
/// Only `<path>` is read, because only `<path>` is what an authored icon is
/// here. Anything else — a `<circle>`, a group with a transform of its own — is
/// refused rather than dropped, so an icon that this cannot draw says so
/// instead of appearing blank.
///
/// The `viewBox` is fitted into the unit square the way SVG itself fits one by
/// default: scaled by its longer side and centred on the shorter one, so the
/// art keeps the proportions it was drawn with.
///
/// # Errors
/// Returns [`SvgError`] for a document this cannot read.
pub fn outline(document: &str) -> Result<Outline, SvgError> {
    let parsed =
        Document::parse(document).map_err(|error| SvgError::Malformed(error.to_string()))?;
    let root = parsed.root_element();
    if root.tag_name().name() != "svg" {
        return Err(SvgError::NotSvg(root.tag_name().name().to_owned()));
    }
    let fit = Fit::read(root.attribute("viewBox").ok_or(SvgError::NoViewBox)?)?;

    let mut rule: Option<FillRule> = None;
    let mut verbs: Vec<Verb> = Vec::new();
    for node in root.descendants().filter(Node::is_element) {
        if node == root {
            continue;
        }
        match node.tag_name().name() {
            "path" => {
                let asked = read_rule(node.attribute("fill-rule"))?;
                match rule {
                    Some(kept) if kept != asked => return Err(SvgError::MixedRules),
                    _ => rule = Some(asked),
                }
                verbs.extend(read_data(
                    node.attribute("d").ok_or(SvgError::NoData)?,
                    fit,
                )?);
            }
            "defs" | "desc" | "g" | "metadata" | "style" | "title" => {}
            other => return Err(SvgError::NotAPath(other.to_owned())),
        }
    }
    if verbs.is_empty() {
        return Err(SvgError::Empty);
    }
    Ok(Outline::new(Path::new(rule.unwrap_or_default(), verbs)))
}

/// How the document's own coordinates reach the unit square.
#[derive(Clone, Copy)]
struct Fit {
    offset: Pt,
    origin: Pt,
    scale: f32,
}

impl Fit {
    fn point(self, point: Point) -> Option<Pt> {
        Some(Pt {
            x: self.offset.x + (point.x.to_f32()? - self.origin.x) * self.scale,
            y: self.offset.y + (point.y.to_f32()? - self.origin.y) * self.scale,
        })
    }

    fn read(view_box: &str) -> Result<Self, SvgError> {
        let mut numbers = view_box
            .split([',', ' ', '\t', '\n', '\r'])
            .filter(|part| !part.is_empty())
            .map(str::parse::<f32>);
        let mut next = || {
            numbers
                .next()
                .and_then(Result::ok)
                .filter(|n| n.is_finite())
        };
        let (Some(x), Some(y), Some(w), Some(h)) = (next(), next(), next(), next()) else {
            return Err(SvgError::ViewBox(view_box.to_owned()));
        };
        if w <= 0.0 || h <= 0.0 || numbers.next().is_some() {
            return Err(SvgError::ViewBox(view_box.to_owned()));
        }
        let side = w.max(h);
        Ok(Self {
            offset: Pt {
                x: (side - w) / (2.0 * side),
                y: (side - h) / (2.0 * side),
            },
            origin: Pt { x, y },
            scale: side.recip(),
        })
    }
}

fn read_rule(attribute: Option<&str>) -> Result<FillRule, SvgError> {
    match attribute {
        None | Some("nonzero") => Ok(FillRule::NonZero),
        Some("evenodd") => Ok(FillRule::EvenOdd),
        Some(other) => Err(SvgError::Rule(other.to_owned())),
    }
}

fn read_data(data: &str, fit: Fit) -> Result<Vec<Verb>, SvgError> {
    let path = BezPath::from_svg(data).map_err(|error| SvgError::Data(error.to_string()))?;
    path.elements()
        .iter()
        .map(|element| verb(*element, fit))
        .collect::<Option<Vec<_>>>()
        .ok_or(SvgError::Coordinate)
}

fn verb(element: PathEl, fit: Fit) -> Option<Verb> {
    Some(match element {
        PathEl::ClosePath => Verb::Close,
        PathEl::CurveTo(first, second, to) => Verb::CurveTo {
            first: fit.point(first)?,
            second: fit.point(second)?,
            to: fit.point(to)?,
        },
        PathEl::LineTo(to) => Verb::LineTo(fit.point(to)?),
        PathEl::MoveTo(to) => Verb::MoveTo(fit.point(to)?),
        PathEl::QuadTo(control, to) => Verb::QuadTo {
            control: fit.point(control)?,
            to: fit.point(to)?,
        },
    })
}
