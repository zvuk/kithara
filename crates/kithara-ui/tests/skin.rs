use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    envelope::{DocKind, probe},
    error::UiDocError,
    ids::{DocId, SourceUri},
    skin::{ColorRole, FontWeight, parse_skin},
};

fn origin() -> SourceUri {
    SourceUri("kithara-dark.kskin.ron".to_owned())
}

#[kithara::test]
fn builtin_skin_parses_every_required_section() {
    let document = parse_skin(builtin::DARK_SKIN, &origin()).unwrap();

    assert_eq!(document.id, DocId("kithara-dark".to_owned()));
    assert_eq!(document.palette.bg, "#12121f");
    assert_eq!(document.layout.grid_gap, 1.0);
    assert_eq!(document.layout.size_gap, 0.0);
    assert_eq!(document.chrome.frame.border, ColorRole::Line);
    assert_eq!(document.palette.line_inner, "#2a2a4c");
    assert_eq!(document.palette.bg_footer, "#1b1b32");
    assert_eq!(document.chrome.header_height, 26.0);
    assert_eq!(document.chrome.chip_pad, 9.0);
    assert_eq!(document.chrome.chip_text_size, 9.0);
    assert_eq!(document.chrome.title_text_size, 11.0);
    assert_eq!(document.chrome.chevron_size, 26.0);
    assert_eq!(document.chrome.footer_height, 22.0);
    assert_eq!(document.chrome.footer_text_size, 9.0);
    assert_eq!(document.chrome.inner_line, ColorRole::LineInner);
    assert_eq!(document.chrome.footer_background, ColorRole::BgFooter);
    assert_eq!(document.text_input.idle_border_width, 0.0);
    assert_eq!(document.knob.body_border, ColorRole::Line);
    assert_eq!(document.knob.drag_range, 140.0);
    assert_eq!(document.vu_stereo.segment_count, 16);
    assert_eq!(document.vu_vertical.warning_threshold, 0.66);
    assert_eq!(document.toggle.active_frame.radius, 0.0);
    assert_eq!(document.checkbox.inactive_frame.border_width, 1.0);
    assert_eq!(document.readout.label.weight, FontWeight::Normal);
    assert_eq!(document.chip.inactive_frame.border, ColorRole::Line);
    assert_eq!(document.button.primary_text.weight, FontWeight::Bold);
    assert_eq!(document.nav.item_height, 30.0);
    assert_eq!(document.nav.marker_width, 2.0);
    assert_eq!(document.nav.icon_size, 14.0);
    assert_eq!(document.nav.text_pad_x, 14.0);
    assert_eq!(document.tab_large.height, 28.0);
    assert_eq!(document.tab_large.pad_y, 0.0);
    assert_eq!(document.tab_large.underline_width, 2.0);
    assert_eq!(document.text.track_title.weight, FontWeight::Semibold);
    assert_eq!(document.fader.handle_frame.radius, 0.0);
    assert_eq!(document.wave.grid_alpha, 0.55);
    assert_eq!(document.deck.header_height, 60.0);
    assert_eq!(document.global_bar.brand_width, 112.0);
    assert_eq!(document.telemetry.percent_scale, 100.0);
    assert_eq!(document.telemetry.percent_width, 3);
    assert_eq!(document.telemetry.scalar_precision, 2);
    assert_eq!(document.track_list.artist_width, 170.0);
    assert_eq!(document.layout_preview.height, 92.0);
    assert_eq!(builtin::skin_doc(), &document);
}

#[kithara::test]
fn skin_envelope_is_probed_as_skin() {
    let envelope = probe(builtin::DARK_SKIN, &origin()).unwrap();

    assert_eq!(envelope.kind, DocKind::Skin);
}

#[kithara::test]
fn unknown_skin_field_is_rejected() {
    let text = builtin::DARK_SKIN.replacen(
        "id: \"kithara-dark\",",
        "id: \"kithara-dark\", unknown: 1,",
        1,
    );
    let error = parse_skin(&text, &origin()).unwrap_err();

    assert!(matches!(error, UiDocError::Syntax { .. }));
}

#[kithara::test]
fn malformed_skin_hex_has_typed_error() {
    let text = builtin::DARK_SKIN.replacen("#12121f", "#12xx1f", 1);
    let error = parse_skin(&text, &origin()).unwrap_err();

    assert!(matches!(
        error,
        UiDocError::BadColor { origin, value }
            if origin == self::origin() && value == "#12xx1f"
    ));
}
