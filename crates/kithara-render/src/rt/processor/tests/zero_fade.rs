use super::*;

#[kithara::test]
fn a_zero_duration_ordinary_fade_reports_its_exact_silence_frame_once() {
    const FADE_AT: usize = BLOCK + 16;
    let (mut mixer, mut ends) = mixer();
    start_dc(&mut mixer, &mut ends, &[A]);
    let fade = send(
        &mut ends,
        at(FADE_AT),
        vec![DeckPart::Fade {
            slot: A,
            settings: CrossfadeSettings::new(0.0, CrossfadeCurve::Linear, 1.0, 0.5)
                .expect("instant fade"),
            dir: FadeDir::Out,
        }],
    );
    assert_eq!(render_frames(&mut mixer, BLOCK, 16), [1.0; 16]);
    assert!(ends.events.drain().next().is_none());
    assert!(receipts(&mut ends).is_empty());
    assert_eq!(render_frames(&mut mixer, FADE_AT, 16), [0.0; 16]);
    let replies = receipts(&mut ends);
    assert_eq!(replies.len(), 1);
    assert_applied_at(outcome_of(&replies, fade), FADE_AT);
    assert!(matches!(replies[0].2.as_slice(), [DeckPart::Fade {
        slot: A, dir: FadeDir::Out, settings,
    }] if settings.duration == 0.0));
    assert_eq!(
        ends.events.drain().collect::<Vec<_>>(),
        [DeckEvent::Faded {
            slot: A,
            at: frame_at(FADE_AT),
        }]
    );
    assert_eq!(mixer.track(A).expect("track").state(), SlotState::Stopped);
    assert_eq!(render_frames(&mut mixer, FADE_AT + 16, 16), [0.0; 16]);
    assert!(ends.events.drain().next().is_none());
    assert!(receipts(&mut ends).is_empty());
}
