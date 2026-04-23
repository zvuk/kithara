//! Raw FFI bindings to Apple `AudioToolbox`.

#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::ffi::c_void;

pub(super) type OSStatus = i32;
pub(super) type AudioFileStreamID = *mut c_void;
pub(super) type AudioFileTypeID = u32;
pub(super) type AudioFileStreamPropertyID = u32;
pub(super) type AudioFormatID = u32;
pub(super) type AudioFormatFlags = u32;
pub(super) type AudioConverterRef = *mut c_void;
pub(super) type UInt32 = u32;
pub(super) type SInt64 = i64;
pub(super) type Float64 = f64;

// AudioStreamPacketDescription
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

// Callback types for AudioFileStream
pub(super) type AudioFileStream_PropertyListenerProc = extern "C" fn(
    inClientData: *mut c_void,
    inAudioFileStream: AudioFileStreamID,
    inPropertyID: AudioFileStreamPropertyID,
    ioFlags: *mut UInt32,
);

pub(super) type AudioFileStream_PacketsProc = extern "C" fn(
    inClientData: *mut c_void,
    inNumberBytes: UInt32,
    inNumberPackets: UInt32,
    inInputData: *const c_void,
    inPacketDescriptions: *mut AudioStreamPacketDescription,
);

// Callback type for AudioConverter
pub(super) type AudioConverterComplexInputDataProc = extern "C" fn(
    inAudioConverter: AudioConverterRef,
    ioNumberDataPackets: *mut UInt32,
    ioData: *mut AudioBufferList,
    outDataPacketDescription: *mut *mut AudioStreamPacketDescription,
    inUserData: *mut c_void,
) -> OSStatus;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    pub(super) fn AudioFileStreamOpen(
        inClientData: *mut c_void,
        inPropertyListenerProc: AudioFileStream_PropertyListenerProc,
        inPacketsProc: AudioFileStream_PacketsProc,
        inFileTypeHint: AudioFileTypeID,
        outAudioFileStream: *mut AudioFileStreamID,
    ) -> OSStatus;

    pub(super) fn AudioFileStreamParseBytes(
        inAudioFileStream: AudioFileStreamID,
        inDataByteSize: UInt32,
        inData: *const c_void,
        inFlags: UInt32,
    ) -> OSStatus;

    pub(super) fn AudioFileStreamGetPropertyInfo(
        inAudioFileStream: AudioFileStreamID,
        inPropertyID: AudioFileStreamPropertyID,
        outPropertyDataSize: *mut UInt32,
        outWritable: *mut u8,
    ) -> OSStatus;

    pub(super) fn AudioFileStreamGetProperty(
        inAudioFileStream: AudioFileStreamID,
        inPropertyID: AudioFileStreamPropertyID,
        ioPropertyDataSize: *mut UInt32,
        outPropertyData: *mut c_void,
    ) -> OSStatus;

    pub(super) fn AudioFileStreamClose(inAudioFileStream: AudioFileStreamID) -> OSStatus;

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
