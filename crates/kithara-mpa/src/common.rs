// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::{
    audio::{Channels, Position},
    codecs::audio::{
        AudioCodecId,
        well_known::{CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3},
    },
    units::Duration,
};

/// The MPEG audio version.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum MpegVersion {
    /// Version 2.5
    Mpeg2p5,
    /// Version 2
    Mpeg2,
    /// Version 1
    Mpeg1,
}

/// The MPEG audio layer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum MpegLayer {
    /// Layer 1
    Layer1,
    /// Layer 2
    Layer2,
    /// Layer 3
    Layer3,
}

/// The channel mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChannelMode {
    /// Single mono audio channel.
    Mono,
    /// Dual mono audio channels.
    DualMono,
    /// Stereo channels.
    Stereo,
    /// Joint Stereo encoded channels (decodes to Stereo).
    JointStereo,
}

impl From<ChannelMode> for usize {
    /// The number of channels the mode decodes to.
    #[inline(always)]
    fn from(mode: ChannelMode) -> Self {
        const STEREO_CHANNELS: usize = 2;

        match mode {
            ChannelMode::Mono => 1,
            _ => STEREO_CHANNELS,
        }
    }
}

impl ChannelMode {
    /// Gets the channel map.
    #[inline(always)]
    pub(crate) fn channels(self) -> Channels {
        let positions = match self {
            Self::Mono => Position::FRONT_LEFT,
            _ => Position::FRONT_LEFT | Position::FRONT_RIGHT,
        };

        Channels::Positioned(positions)
    }
}

/// An MPEG 1, 2, or 2.5 audio frame header.
#[derive(Debug)]
pub(crate) struct FrameHeader {
    pub(crate) channel_mode: ChannelMode,
    pub(crate) layer: MpegLayer,
    pub(crate) version: MpegVersion,
    pub(crate) has_crc: bool,
    pub(crate) bitrate: u32,
    pub(crate) sample_rate: u32,
    pub(crate) frame_size: usize,
}

impl FrameHeader {
    /// Returns the codec ID for the frame.
    pub(crate) fn codec(&self) -> AudioCodecId {
        match self.layer {
            MpegLayer::Layer1 => CODEC_ID_MP1,
            MpegLayer::Layer2 => CODEC_ID_MP2,
            MpegLayer::Layer3 => CODEC_ID_MP3,
        }
    }

    /// Returns the duration of the MPEG frame.
    ///
    /// This is effectively the same as `num_frames`, but as a `Duration`.
    #[inline(always)]
    pub(crate) fn duration(&self) -> Duration {
        Duration::from(self.num_frames())
    }

    /// Returns true if this is an MPEG-1 frame, false otherwise.
    #[inline(always)]
    pub(crate) fn is_mpeg1(&self) -> bool {
        self.version == MpegVersion::Mpeg1
    }

    /// Returns the number of channels per granule.
    #[inline(always)]
    pub(crate) fn n_channels(&self) -> usize {
        usize::from(self.channel_mode)
    }

    /// Returns the number of granules in the frame.
    #[inline(always)]
    pub(crate) fn n_granules(&self) -> u16 {
        const MPEG1_GRANULES: u16 = 2;

        match self.version {
            MpegVersion::Mpeg1 => MPEG1_GRANULES,
            _ => 1,
        }
    }

    /// Returns the number of per-channel audio samples in the MPEG frame.
    pub(crate) fn num_frames(&self) -> u16 {
        const LAYER1_SAMPLES: u16 = 384;
        const LAYER2_SAMPLES: u16 = 1152;
        const LAYER3_SAMPLES_PER_GRANULE: u16 = 576;

        match self.layer {
            MpegLayer::Layer1 => LAYER1_SAMPLES,
            MpegLayer::Layer2 => LAYER2_SAMPLES,
            MpegLayer::Layer3 => LAYER3_SAMPLES_PER_GRANULE * self.n_granules(),
        }
    }

    /// Get the side information length.
    #[inline(always)]
    pub(crate) fn side_info_len(&self) -> usize {
        const MPEG1_MONO_SIDE_INFO_LEN: usize = 17;
        const MPEG1_STEREO_SIDE_INFO_LEN: usize = 32;
        const MPEG2_MONO_SIDE_INFO_LEN: usize = 9;
        const MPEG2_STEREO_SIDE_INFO_LEN: usize = 17;

        match (self.version, self.channel_mode) {
            (MpegVersion::Mpeg1, ChannelMode::Mono) => MPEG1_MONO_SIDE_INFO_LEN,
            (MpegVersion::Mpeg1, _) => MPEG1_STEREO_SIDE_INFO_LEN,
            (_, ChannelMode::Mono) => MPEG2_MONO_SIDE_INFO_LEN,
            (_, _) => MPEG2_STEREO_SIDE_INFO_LEN,
        }
    }
}
