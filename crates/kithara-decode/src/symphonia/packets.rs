use symphonia_core::{
    errors::Result,
    formats::{
        FormatReader, SeekMode, SeekTo, SeekedTo,
        well_known::{FORMAT_ID_MP1, FORMAT_ID_MP2, FORMAT_ID_MP3},
    },
    packet::Packet,
    units::{Duration, Timestamp},
};

pub(super) struct PacketInfo {
    pub(super) track_id: u32,
    pub(super) pts: Timestamp,
    pub(super) dur: Duration,
}

pub(super) struct Packets {
    reader: Box<dyn FormatReader>,
    packet: Option<Packet>,
}

impl Packets {
    pub(super) fn new(reader: Box<dyn FormatReader>) -> Self {
        Self {
            reader,
            packet: None,
        }
    }

    pub(super) fn restores_interrupted_packet(&self) -> bool {
        matches!(
            self.reader.format_info().format,
            FORMAT_ID_MP1 | FORMAT_ID_MP2 | FORMAT_ID_MP3
        )
    }

    pub(super) fn read(&mut self) -> Result<Option<PacketInfo>> {
        self.packet = self.reader.next_packet()?;
        Ok(self.packet.as_ref().map(|packet| PacketInfo {
            track_id: packet.track_id,
            pts: packet.pts,
            dur: packet.dur,
        }))
    }

    pub(super) fn data(&self) -> &[u8] {
        &self
            .packet
            .as_ref()
            .expect("packet was read successfully")
            .data
    }

    pub(super) fn seek(&mut self, mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        self.reader.seek(mode, to)
    }
}
