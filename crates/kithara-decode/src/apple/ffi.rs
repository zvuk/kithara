#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::ffi::c_void;

pub(crate) type OSStatus = i32;
pub(crate) type AudioFormatID = u32;
pub(crate) type AudioFormatFlags = u32;
pub(crate) type AudioConverterRef = *mut c_void;
pub(crate) type UInt32 = u32;
pub(crate) type Float64 = f64;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AudioStreamPacketDescription {
    pub(crate) mStartOffset: i64,
    pub(crate) mVariableFramesInPacket: UInt32,
    pub(crate) mDataByteSize: UInt32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AudioStreamBasicDescription {
    pub(crate) mSampleRate: Float64,
    pub(crate) mFormatID: AudioFormatID,
    pub(crate) mFormatFlags: AudioFormatFlags,
    pub(crate) mBytesPerPacket: UInt32,
    pub(crate) mFramesPerPacket: UInt32,
    pub(crate) mBytesPerFrame: UInt32,
    pub(crate) mChannelsPerFrame: UInt32,
    pub(crate) mBitsPerChannel: UInt32,
    pub(crate) mReserved: UInt32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct AudioBuffer {
    pub(crate) mNumberChannels: UInt32,
    pub(crate) mDataByteSize: UInt32,
    pub(crate) mData: *mut c_void,
}

#[repr(C)]
pub(crate) struct AudioBufferList {
    pub(crate) mNumberBuffers: UInt32,
    pub(crate) mBuffers: [AudioBuffer; 1],
}

/// `kAudioConverterPrimeInfo` payload — codec-reported encoder priming
/// in PCM frames. Populated by `AudioConverterGetProperty` after the
/// converter has consumed at least one input packet.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AudioConverterPrimeInfo {
    pub(crate) leading_frames: UInt32,
    pub(crate) trailing_frames: UInt32,
}

/// Callback type for `AudioConverterFillComplexBuffer`.
pub(crate) type AudioConverterComplexInputDataProc = extern "C" fn(
    inAudioConverter: AudioConverterRef,
    ioNumberDataPackets: *mut UInt32,
    ioData: *mut AudioBufferList,
    outDataPacketDescription: *mut *mut AudioStreamPacketDescription,
    inUserData: *mut c_void,
) -> OSStatus;

pub(crate) type AudioFormatPropertyID = u32;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct AudioFormatInfo {
    pub(crate) mASBD: AudioStreamBasicDescription,
    pub(crate) mMagicCookie: *const c_void,
    pub(crate) mMagicCookieSize: UInt32,
}

pub(crate) type AudioChannelLayoutTag = u32;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AudioFormatListItem {
    pub(crate) mASBD: AudioStreamBasicDescription,
    pub(crate) mChannelLayoutTag: AudioChannelLayoutTag,
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    pub(crate) fn AudioFormatGetPropertyInfo(
        inPropertyID: AudioFormatPropertyID,
        inSpecifierSize: UInt32,
        inSpecifier: *const c_void,
        outPropertyDataSize: *mut UInt32,
    ) -> OSStatus;

    pub(crate) fn AudioFormatGetProperty(
        inPropertyID: AudioFormatPropertyID,
        inSpecifierSize: UInt32,
        inSpecifier: *const c_void,
        ioPropertyDataSize: *mut UInt32,
        outPropertyData: *mut c_void,
    ) -> OSStatus;
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    pub(crate) fn AudioConverterNew(
        inSourceFormat: *const AudioStreamBasicDescription,
        inDestinationFormat: *const AudioStreamBasicDescription,
        outAudioConverter: *mut AudioConverterRef,
    ) -> OSStatus;

    pub(crate) fn AudioConverterSetProperty(
        inAudioConverter: AudioConverterRef,
        inPropertyID: u32,
        inPropertyDataSize: UInt32,
        inPropertyData: *const c_void,
    ) -> OSStatus;

    pub(crate) fn AudioConverterGetProperty(
        inAudioConverter: AudioConverterRef,
        inPropertyID: u32,
        ioPropertyDataSize: *mut UInt32,
        outPropertyData: *mut c_void,
    ) -> OSStatus;

    pub(crate) fn AudioConverterFillComplexBuffer(
        inAudioConverter: AudioConverterRef,
        inInputDataProc: AudioConverterComplexInputDataProc,
        inInputDataProcUserData: *mut c_void,
        ioOutputDataPacketSize: *mut UInt32,
        outOutputData: *mut AudioBufferList,
        outPacketDescription: *mut AudioStreamPacketDescription,
    ) -> OSStatus;

    pub(crate) fn AudioConverterDispose(inAudioConverter: AudioConverterRef) -> OSStatus;

    pub(crate) fn AudioConverterReset(inAudioConverter: AudioConverterRef) -> OSStatus;
}

pub(crate) type AudioFileID = *mut c_void;
pub(crate) type AudioFilePropertyID = u32;
pub(crate) type AudioFileTypeID = u32;
pub(crate) type SInt64 = i64;

pub(crate) type AudioFile_ReadProc = extern "C" fn(
    inClientData: *mut c_void,
    inPosition: SInt64,
    requestCount: UInt32,
    buffer: *mut c_void,
    actualCount: *mut UInt32,
) -> OSStatus;

pub(crate) type AudioFile_GetSizeProc = extern "C" fn(inClientData: *mut c_void) -> SInt64;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    pub(crate) fn AudioFileOpenWithCallbacks(
        inClientData: *mut c_void,
        inReadFunc: AudioFile_ReadProc,
        inWriteFunc: *const c_void,
        inGetSizeFunc: AudioFile_GetSizeProc,
        inSetSizeFunc: *const c_void,
        inFileTypeHint: AudioFileTypeID,
        outAudioFile: *mut AudioFileID,
    ) -> OSStatus;

    pub(crate) fn AudioFileClose(inAudioFile: AudioFileID) -> OSStatus;

    pub(crate) fn AudioFileGetProperty(
        inAudioFile: AudioFileID,
        inPropertyID: AudioFilePropertyID,
        ioDataSize: *mut UInt32,
        outPropertyData: *mut c_void,
    ) -> OSStatus;

    pub(crate) fn AudioFileGetPropertyInfo(
        inAudioFile: AudioFileID,
        inPropertyID: AudioFilePropertyID,
        outDataSize: *mut UInt32,
        isWritable: *mut UInt32,
    ) -> OSStatus;

    pub(crate) fn AudioFileReadPacketData(
        inAudioFile: AudioFileID,
        inUseCache: u8,
        ioNumBytes: *mut UInt32,
        outPacketDescriptions: *mut AudioStreamPacketDescription,
        inStartingPacket: SInt64,
        ioNumPackets: *mut UInt32,
        outBuffer: *mut c_void,
    ) -> OSStatus;
}
