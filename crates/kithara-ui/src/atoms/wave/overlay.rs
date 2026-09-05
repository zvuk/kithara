use crate::{
    atoms::design::quad::quad,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    shaping::TextContext,
    skin::{TextRoleSkin, WaveOverlaySkin},
};

#[derive(Clone, Copy)]
pub(crate) struct Overlay<'a> {
    pub(crate) artist: &'a str,
    pub(crate) badge: &'a str,
    pub(crate) bpm: &'a str,
    pub(crate) key: &'a str,
    pub(crate) remain: &'a str,
    pub(crate) title: &'a str,
    pub(crate) palette: OverlayPalette,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct OverlayPalette {
    pub(crate) art_background: Rgba,
    pub(crate) art_border: Rgba,
    pub(crate) art_label: Rgba,
    pub(crate) artist: Rgba,
    pub(crate) background: Rgba,
    pub(crate) badge_background: Rgba,
    pub(crate) badge_border: Rgba,
    pub(crate) badge_text: Rgba,
    pub(crate) bpm: Rgba,
    pub(crate) key: Rgba,
    pub(crate) readout_background: Rgba,
    pub(crate) readout_border: Rgba,
    pub(crate) readout_label: Rgba,
    pub(crate) remain: Rgba,
    pub(crate) title: Rgba,
}

pub(crate) fn strip(bounds: Rect, metrics: WaveOverlaySkin) -> Rect {
    Rect {
        h: metrics.height.min(bounds.h),
        w: bounds.w,
        x: bounds.x,
        y: bounds.y,
    }
}

pub(crate) fn paint(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    bounds: Rect,
    data: Overlay<'_>,
    metrics: WaveOverlaySkin,
) {
    let header = strip(bounds, metrics);
    list.fill_rect(header, data.palette.background);
    let summary_x = draw_art(list, text, header, metrics, data.palette);
    let telemetry_x = draw_telemetry(list, text, header, data, metrics);
    draw_summary(
        list,
        text,
        header,
        Rect {
            h: (header.h - metrics.padding_y * 2.0).max(0.0),
            w: (telemetry_x - metrics.gap - summary_x).max(0.0),
            x: summary_x,
            y: header.y + metrics.padding_y,
        },
        data,
        metrics,
    );
}

fn draw_art(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    header: Rect,
    metrics: WaveOverlaySkin,
    palette: OverlayPalette,
) -> f32 {
    let art = Rect {
        h: metrics.art_size,
        w: metrics.art_size,
        x: header.x + metrics.padding_x,
        y: header.y + (header.h - metrics.art_size) / 2.0,
    };
    quad(
        list,
        art,
        metrics.art_frame,
        palette.art_background,
        palette.art_border,
    );
    draw_centered(list, text, "ART", art, metrics.art_label, palette.art_label);
    art.x + art.w + metrics.gap
}

fn draw_telemetry(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    header: Rect,
    data: Overlay<'_>,
    metrics: WaveOverlaySkin,
) -> f32 {
    let readout_height = metrics
        .readout_height
        .min((header.h - metrics.padding_y * 2.0).max(0.0));
    let readout_y = header.y + (header.h - readout_height) / 2.0;
    let mut right = header.x + header.w - metrics.padding_x;
    let badge = Rect {
        h: metrics.badge_size,
        w: metrics.badge_size,
        x: right - metrics.badge_size,
        y: header.y + (header.h - metrics.badge_size) / 2.0,
    };
    quad(
        list,
        badge,
        metrics.badge_frame,
        data.palette.badge_background,
        data.palette.badge_border,
    );
    draw_centered(
        list,
        text,
        data.badge,
        badge,
        metrics.badge_text,
        data.palette.badge_text,
    );
    right = badge.x - metrics.gap;

    let remain = readout_rect(right, metrics.remain_width, readout_y, readout_height);
    draw_readout(
        list,
        text,
        remain,
        ("REMAIN", data.remain, data.palette.remain, false),
        metrics,
        data.palette,
    );
    right = remain.x - metrics.gap;

    let key = readout_rect(right, metrics.key_width, readout_y, readout_height);
    draw_readout(
        list,
        text,
        key,
        ("KEY", data.key, data.palette.key, true),
        metrics,
        data.palette,
    );
    right = key.x - metrics.gap;

    let bpm = readout_rect(right, metrics.bpm_width, readout_y, readout_height);
    draw_readout(
        list,
        text,
        bpm,
        ("BPM", data.bpm, data.palette.bpm, true),
        metrics,
        data.palette,
    );
    bpm.x
}

const fn readout_rect(right: f32, width: f32, y: f32, height: f32) -> Rect {
    Rect {
        y,
        h: height,
        w: width,
        x: right - width,
    }
}

fn draw_summary(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    header: Rect,
    bounds: Rect,
    data: Overlay<'_>,
    metrics: WaveOverlaySkin,
) {
    let total_text_height = metrics.title.size + metrics.summary_gap + metrics.artist.size;
    let title_y = header.y + (header.h - total_text_height) / 2.0;
    let mut clipped = list.child();
    draw_left(
        &mut clipped,
        text,
        data.title,
        Pt {
            x: bounds.x,
            y: title_y,
        },
        bounds.w,
        metrics.title,
        data.palette.title,
    );
    draw_left(
        &mut clipped,
        text,
        data.artist,
        Pt {
            x: bounds.x,
            y: title_y + metrics.title.size + metrics.summary_gap,
        },
        bounds.w,
        metrics.artist,
        data.palette.artist,
    );
    list.clip(bounds, clipped.finish());
}

fn draw_readout(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    bounds: Rect,
    data: (&str, &str, Rgba, bool),
    metrics: WaveOverlaySkin,
    palette: OverlayPalette,
) {
    let (label, value, value_color, framed) = data;
    if framed {
        quad(
            list,
            bounds,
            metrics.readout_frame,
            palette.readout_background,
            palette.readout_border,
        );
    }
    let inner_height = (bounds.h - metrics.readout_padding_y * 2.0).max(0.0);
    let total_height =
        metrics.readout_label.size + metrics.readout_gap + metrics.readout_value.size;
    let label_y = bounds.y + metrics.readout_padding_y + (inner_height - total_height) / 2.0;
    let right = bounds.x + bounds.w - metrics.readout_padding_x;
    let max_width = (bounds.w - metrics.readout_padding_x * 2.0).max(0.0);
    draw_right(
        list,
        text,
        label,
        Pt {
            x: right,
            y: label_y,
        },
        max_width,
        metrics.readout_label,
        palette.readout_label,
    );
    draw_right(
        list,
        text,
        value,
        Pt {
            x: right,
            y: label_y + metrics.readout_label.size + metrics.readout_gap,
        },
        max_width,
        metrics.readout_value,
        value_color,
    );
}

fn draw_centered(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    content: &str,
    bounds: Rect,
    role: TextRoleSkin,
    color: Rgba,
) {
    let run = text.shape(content, role, Some(bounds.w));
    list.text(
        &run,
        content,
        Transform::translate(Pt {
            x: bounds.x + (bounds.w - run.width()) / 2.0,
            y: bounds.y + (bounds.h - run.height()) / 2.0,
        }),
        color,
    );
}

fn draw_left(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    content: &str,
    position: Pt,
    max_width: f32,
    role: TextRoleSkin,
    color: Rgba,
) {
    draw_aligned::<false>(list, text, content, position, max_width, role, color);
}

fn draw_right(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    content: &str,
    position: Pt,
    max_width: f32,
    role: TextRoleSkin,
    color: Rgba,
) {
    draw_aligned::<true>(list, text, content, position, max_width, role, color);
}

fn draw_aligned<const RIGHT: bool>(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    content: &str,
    position: Pt,
    max_width: f32,
    role: TextRoleSkin,
    color: Rgba,
) {
    let run = text.shape(content, role, Some(max_width));
    let x = position.x - if RIGHT { run.width() } else { 0.0 };
    list.text(
        &run,
        content,
        Transform::translate(Pt { x, y: position.y }),
        color,
    );
}
