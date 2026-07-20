use num_traits::cast::AsPrimitive;

/// Whole track fits the widget; the smallest meaningful zoom.
pub(crate) const MIN_ZOOM: f32 = 1.0;

/// Deepest zoom; the data is native-resolution so this is just a practical cap.
pub(crate) const MAX_ZOOM: f32 = 40.0;

/// Multiplier applied by the on-screen `−` / `+` buttons per click.
pub(crate) const ZOOM_STEP: f32 = 1.5;

/// A zoom/pan request from the canvas or the `−`/`+` control.
#[derive(Debug, Clone, Copy)]
pub(crate) enum WaveMsg {
    /// Wheel zoom anchored at a screen fraction in `[0, 1]` (toward cursor).
    ZoomBy { factor: f32, anchor: f32 },
    /// Button zoom: multiply zoom, re-centered on the viewport middle.
    Zoom(f32),
    /// Drag pan by a screen-fraction delta.
    Pan(f32),
}

/// Normalized zoom/pan window over the track `[0, 1]`, pixel- and iced-free.
/// Every accessor normalizes first, so any `(zoom, offset)` maps to a valid
/// window without panicking. Bucket `i` of `n` spans `[i/n, (i+1)/n]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Viewport {
    offset: f32,
    zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            offset: 0.0,
            zoom: MIN_ZOOM,
        }
    }
}

impl Viewport {
    /// Apply a [`WaveMsg`] transition, yielding the next viewport.
    pub(crate) fn apply(self, msg: WaveMsg) -> Self {
        match msg {
            WaveMsg::ZoomBy { factor, anchor } => self.zoom_by(factor, anchor),
            WaveMsg::Zoom(factor) => self.zoom_by(factor, 0.5),
            WaveMsg::Pan(delta) => self.pan(delta),
        }
    }

    /// Clamp zoom into range, then offset into `[0, 1 - 1/zoom]`. Order matters:
    /// deriving the offset bound from the clamped zoom keeps it non-negative.
    fn normalized(self) -> Self {
        let zoom = if self.zoom.is_finite() {
            self.zoom.clamp(MIN_ZOOM, MAX_ZOOM)
        } else {
            MIN_ZOOM
        };
        let max_offset = (1.0 - 1.0 / zoom).max(0.0);
        let offset = if self.offset.is_finite() {
            self.offset.clamp(0.0, max_offset)
        } else {
            0.0
        };
        Self { offset, zoom }
    }

    /// Shift the window by a screen-fraction drag delta. Dragging right
    /// (positive delta) reveals earlier content, so offset decreases.
    pub(crate) fn pan(self, delta_screen_frac: f32) -> Self {
        let cur = self.normalized();
        let delta = if delta_screen_frac.is_finite() {
            delta_screen_frac
        } else {
            0.0
        };
        Self {
            offset: cur.offset - delta / cur.zoom,
            zoom: cur.zoom,
        }
        .normalized()
    }

    /// Bucket range `[lo, hi)` covering pixel column `[x, x+1]` of a `w`-wide
    /// canvas.
    pub(crate) fn pixel_buckets(self, x: f32, w: f32, n: usize) -> (usize, usize) {
        if n == 0 || w <= 0.0 {
            return (0, 0);
        }
        let nf: f32 = n.as_();
        let lo: usize = (self.track_frac(x / w) * nf).floor().max(0.0).as_();
        let hi: usize = (self.track_frac((x + 1.0) / w) * nf).ceil().max(0.0).as_();
        let lo = lo.min(n - 1);
        (lo, hi.clamp(lo + 1, n))
    }

    /// Map a track fraction to a screen fraction. May fall outside `[0, 1]`
    /// when the point is off-screen (e.g. a playhead past the visible window).
    pub(crate) fn screen_frac(self, track_frac: f32) -> f32 {
        let cur = self.normalized();
        (track_frac - cur.offset) * cur.zoom
    }

    /// Map a screen fraction in `[0, 1]` to a track fraction.
    pub(crate) fn track_frac(self, screen_frac: f32) -> f32 {
        let cur = self.normalized();
        cur.offset + screen_frac / cur.zoom
    }

    /// Clamped zoom level, for the on-screen `N×` readout.
    pub(crate) fn zoom(self) -> f32 {
        self.normalized().zoom
    }

    /// Multiply zoom by `factor`, keeping the track point under `anchor` (screen
    /// fraction) pinned — zoom toward the cursor. Pin slips only at track edges.
    pub(crate) fn zoom_by(self, factor: f32, anchor: f32) -> Self {
        let cur = self.normalized();
        let anchor = if anchor.is_finite() {
            anchor.clamp(0.0, 1.0)
        } else {
            0.5
        };
        let factor = if factor.is_finite() && factor > 0.0 {
            factor
        } else {
            1.0
        };
        let pinned = cur.offset + anchor / cur.zoom;
        let new_zoom = (cur.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        Self {
            zoom: new_zoom,
            offset: pinned - anchor / new_zoom,
        }
        .normalized()
    }
}
