//! Raw FFI bindings to Apple `AudioToolbox`.
//!
//! Codec-only surface — `AudioConverter` plus the `AudioStreamBasicDescription`
//! / `AudioBuffer` plumbing used by [`crate::codec::AppleCodec`] to feed
//! demuxed frames through the hardware codec.

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

/// Callback type for `AudioConverterFillComplexBuffer`.
pub(crate) type AudioConverterComplexInputDataProc = extern "C" fn(
    inAudioConverter: AudioConverterRef,
    ioNumberDataPackets: *mut UInt32,
    ioData: *mut AudioBufferList,
    outDataPacketDescription: *mut *mut AudioStreamPacketDescription,
    inUserData: *mut c_void,
) -> OSStatus;

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
