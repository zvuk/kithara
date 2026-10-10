use kithara_decode::DecoderBackend;
use kithara_events::DeferredBus;
use kithara_signal::AudioSpec;
use kithara_stream::MediaInfo;

use super::DecoderGeneration;
use crate::{
    AudioEvent, AudioLaneEvent, DecoderChangeCause, DecoderEvent, FrameDomain,
    audio::event::{
        DecoderChangedEventData, decoder_changed_event, decoder_gapless_event,
        map_playback_resampler_kind, map_resampler_kind,
    },
};

pub(crate) struct GenerationInstalled<'a> {
    pub(crate) generation: &'a DecoderGeneration,
    pub(crate) playback_resampler_backend: &'static str,
    pub(crate) backend: DecoderBackend,
    pub(crate) cause: DecoderChangeCause,
    pub(crate) recreates_on_route: bool,
    pub(crate) host_sample_rate: u32,
}

pub(crate) fn enqueue_generation_installed(
    emit: &DeferredBus<AudioLaneEvent>,
    installed: &GenerationInstalled<'_>,
) {
    let &GenerationInstalled {
        backend,
        cause,
        generation,
        host_sample_rate,
        playback_resampler_backend,
        recreates_on_route,
    } = installed;
    let decoder = generation.decoder();
    let duration = decoder.duration();
    let media_info = generation.media_info();
    let spec = decoder.spec();
    let track_info = decoder.track_info();
    emit.enqueue(decoder_changed_event(DecoderChangedEventData {
        backend,
        media_info,
        spec,
        cause,
        duration,
        track_info: &track_info,
        base_offset: generation.base_offset(),
    }));
    if let Some(event) = decoder_gapless_event(media_info, spec, &track_info, FrameDomain::Output) {
        emit.enqueue(event);
    }
    if let Some(event) = decoder_resampler_event(
        media_info,
        spec,
        host_sample_rate,
        playback_resampler_backend,
        recreates_on_route,
    ) {
        emit.enqueue(event);
    }
    if let Some(event) = playback_resampler_event(
        media_info,
        spec,
        host_sample_rate,
        playback_resampler_backend,
    ) {
        emit.enqueue(event);
    }
}

fn decoder_resampler_event(
    media_info: Option<&MediaInfo>,
    spec: AudioSpec,
    output_rate: u32,
    backend: &'static str,
    recreates_on_route: bool,
) -> Option<DecoderEvent> {
    if !recreates_on_route || output_rate == 0 {
        return None;
    }
    let input_rate = media_info
        .and_then(|info| info.sample_rate)
        .or_else(|| (spec.sample_rate.get() == output_rate).then_some(spec.sample_rate.get()))?;
    Some(DecoderEvent::ResamplerConfigured {
        backend: map_resampler_kind(backend),
        input_rate,
        output_rate,
        channels: spec.channels,
        bypassed: input_rate == output_rate,
    })
}

fn playback_resampler_event(
    media_info: Option<&MediaInfo>,
    spec: AudioSpec,
    host_sample_rate: u32,
    backend: &'static str,
) -> Option<AudioEvent> {
    if host_sample_rate == 0 {
        return None;
    }
    let source_sample_rate = media_info.and_then(|info| info.sample_rate).or_else(|| {
        (spec.sample_rate.get() == host_sample_rate).then_some(spec.sample_rate.get())
    })?;
    Some(AudioEvent::PlaybackResamplerConfigured {
        backend: map_playback_resampler_kind(backend),
        host_sample_rate,
        source_sample_rate,
        active: host_sample_rate != source_sample_rate && backend != "none",
    })
}
