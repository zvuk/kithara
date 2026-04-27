//! Raw FFI bindings to Apple `AudioToolbox`.
//!
//! The backend parses containers through `AudioFile` (opened with
//! callbacks so we can feed an arbitrary `Read + Seek` source) and
//! decodes compressed packets to PCM through `AudioConverter`.

#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::ffi::c_void;

pub(super) type OSStatus = i32;
pub(super) type AudioFileID = *mut c_void;
pub(super) type AudioFileTypeID = u32;
pub(super) type AudioFilePropertyID = u32;
pub(super) type AudioFormatID = u32;
pub(super) type AudioFormatFlags = u32;
pub(super) type AudioConverterRef = *mut c_void;
pub(super) type UInt32 = u32;
pub(super) type SInt64 = i64;
pub(super) type Float64 = f64;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AudioStreamPacketDescription {
    pub(super) mStartOffset: SInt64,
    pub(super) mVariableFramesInPacket: UInt32,
    pub(super) mDataByteSize: UInt32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AudioStreamBasicDescription {
    pub(super) mSampleRate: Float64,
    pub(super) mFormatID: AudioFormatID,
    pub(super) mFormatFlags: AudioFormatFlags,
    pub(super) mBytesPerPacket: UInt32,
    pub(super) mFramesPerPacket: UInt32,
    pub(super) mBytesPerFrame: UInt32,
    pub(super) mChannelsPerFrame: UInt32,
    pub(super) mBitsPerChannel: UInt32,
    pub(super) mReserved: UInt32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct AudioBuffer {
    pub(super) mNumberChannels: UInt32,
    pub(super) mDataByteSize: UInt32,
    pub(super) mData: *mut c_void,
}

#[repr(C)]
pub(super) struct AudioBufferList {
    pub(super) mNumberBuffers: UInt32,
    pub(super) mBuffers: [AudioBuffer; 1],
}

/// Translation between frame index and packet index (VBR containers).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AudioFramePacketTranslation {
    pub(super) mFrame: SInt64,
    pub(super) mPacket: SInt64,
    pub(super) mFrameOffsetInPacket: UInt32,
}

/// Translation between absolute byte offset and packet index (VBR containers).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AudioBytePacketTranslation {
    pub(super) mByte: SInt64,
    pub(super) mPacket: SInt64,
    pub(super) mByteOffsetInPacket: UInt32,
    pub(super) mFlags: UInt32,
}

/// Callbacks supplied to `AudioFileOpenWithCallbacks`. `inClientData`
/// receives the pointer passed to `AudioFileOpenWithCallbacks`.
pub(super) type AudioFile_ReadProc = extern "C" fn(
    inClientData: *mut c_void,
    inPosition: SInt64,
    requestCount: UInt32,
    buffer: *mut c_void,
    actualCount: *mut UInt32,
) -> OSStatus;

pub(super) type AudioFile_GetSizeProc = extern "C" fn(inClientData: *mut c_void) -> SInt64;

/// Callback type for `AudioConverterFillComplexBuffer`.
pub(super) type AudioConverterComplexInputDataProc = extern "C" fn(
    inAudioConverter: AudioConverterRef,
    ioNumberDataPackets: *mut UInt32,
    ioData: *mut AudioBufferList,
    outDataPacketDescription: *mut *mut AudioStreamPacketDescription,
    inUserData: *mut c_void,
) -> OSStatus;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    pub(super) fn AudioFileOpenWithCallbacks(
        inClientData: *mut c_void,
        inReadFunc: AudioFile_ReadProc,
        inWriteFunc: *const c_void,
        inGetSizeFunc: AudioFile_GetSizeProc,
        inSetSizeFunc: *const c_void,
        inFileTypeHint: AudioFileTypeID,
        outAudioFile: *mut AudioFileID,
    ) -> OSStatus;

    pub(super) fn AudioFileClose(inAudioFile: AudioFileID) -> OSStatus;

    pub(super) fn AudioFileGetPropertyInfo(
        inAudioFile: AudioFileID,
        inPropertyID: AudioFilePropertyID,
        outDataSize: *mut UInt32,
        isWritable: *mut UInt32,
    ) -> OSStatus;

    pub(super) fn AudioFileGetProperty(
        inAudioFile: AudioFileID,
        inPropertyID: AudioFilePropertyID,
        ioDataSize: *mut UInt32,
        outPropertyData: *mut c_void,
    ) -> OSStatus;

    pub(super) fn AudioFileReadPacketData(
        inAudioFile: AudioFileID,
        inUseCache: u8,
        ioNumBytes: *mut UInt32,
        outPacketDescriptions: *mut AudioStreamPacketDescription,
        inStartingPacket: SInt64,
        ioNumPackets: *mut UInt32,
        outBuffer: *mut c_void,
    ) -> OSStatus;

    pub(super) fn AudioConverterNew(
        inSourceFormat: *const AudioStreamBasicDescription,
        inDestinationFormat: *const AudioStreamBasicDescription,
        outAudioConverter: *mut AudioConverterRef,
    ) -> OSStatus;

    pub(super) fn AudioConverterSetProperty(
        inAudioConverter: AudioConverterRef,
        inPropertyID: u32,
        inPropertyDataSize: UInt32,
        inPropertyData: *const c_void,
    ) -> OSStatus;

    pub(super) fn AudioConverterFillComplexBuffer(
        inAudioConverter: AudioConverterRef,
        inInputDataProc: AudioConverterComplexInputDataProc,
        inInputDataProcUserData: *mut c_void,
        ioOutputDataPacketSize: *mut UInt32,
        outOutputData: *mut AudioBufferList,
        outPacketDescription: *mut AudioStreamPacketDescription,
    ) -> OSStatus;

    pub(super) fn AudioConverterDispose(inAudioConverter: AudioConverterRef) -> OSStatus;

    pub(super) fn AudioConverterReset(inAudioConverter: AudioConverterRef) -> OSStatus;
}
