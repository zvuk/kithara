use std::{
    ffi::c_void,
    mem::size_of,
    ptr::{self, NonNull},
    slice,
};

use super::{
    pod::ApplePod,
    sys::{
        AudioFileID, AudioFileTypeID, AudioStreamPacketDescription, NO_ERR, OSStatus, PARAM_ERR,
        SInt64, UInt32, audio_file_close, audio_file_get_property_info_raw,
        audio_file_get_property_raw, audio_file_open_with_callbacks, audio_file_read_packet_data,
    },
};

pub trait AudioFileCallbacks: Send + 'static {
    /// Reads bytes at `position` into `buffer`.
    ///
    /// # Errors
    ///
    /// Returns an `AudioToolbox` status when the backing reader cannot provide data.
    fn read_at(&mut self, position: SInt64, buffer: &mut [u8]) -> Result<UInt32, OSStatus>;

    fn size(&self) -> SInt64;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioFilePacketRead {
    pub bytes: UInt32,
    pub packets: UInt32,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct AudioFile<C>
where
    C: AudioFileCallbacks,
{
    raw: NonNull<c_void>,
    #[field(get)]
    callbacks: Box<C>,
}

// SAFETY: AudioFileID is an opaque Apple handle and callback owner is Send.
// SAFETY: handle operations require &mut self or &self.
unsafe impl<C> Send for AudioFile<C> where C: AudioFileCallbacks {}

impl<C> AudioFile<C>
where
    C: AudioFileCallbacks,
{
    /// Reads a typed `AudioFile` property.
    ///
    /// # Errors
    ///
    /// Returns the `AudioToolbox` status when the property cannot be read.
    pub fn get_property<T>(&self, property: u32) -> Result<T, OSStatus>
    where
        T: ApplePod,
    {
        let mut value = T::default();
        let mut size = UInt32::try_from(size_of::<T>()).map_err(|_| PARAM_ERR)?;
        // SAFETY: self.raw is live; value is writable storage for size bytes.
        let status = unsafe {
            audio_file_get_property_raw(
                self.raw.as_ptr(),
                property,
                &mut size,
                ptr::from_mut(&mut value).cast::<c_void>(),
            )
        };
        if status == NO_ERR {
            Ok(value)
        } else {
            Err(status)
        }
    }

    /// Reads an `AudioFile` property as raw bytes.
    ///
    /// # Errors
    ///
    /// Returns the `AudioToolbox` status when the property cannot be read.
    pub fn get_property_bytes(&self, property: u32) -> Result<Vec<u8>, OSStatus> {
        let (mut size, _) = self.get_property_info(property)?;
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut bytes = vec![0_u8; usize::try_from(size).map_err(|_| PARAM_ERR)?];
        // SAFETY: self.raw is live; bytes has size bytes of writable storage.
        let status = unsafe {
            audio_file_get_property_raw(
                self.raw.as_ptr(),
                property,
                &mut size,
                bytes.as_mut_ptr().cast::<c_void>(),
            )
        };
        if status == NO_ERR {
            bytes.truncate(usize::try_from(size).map_err(|_| PARAM_ERR)?);
            Ok(bytes)
        } else {
            Err(status)
        }
    }

    /// Reads `AudioFile` property metadata.
    ///
    /// # Errors
    ///
    /// Returns the `AudioToolbox` status when the property info cannot be read.
    pub fn get_property_info(&self, property: u32) -> Result<(UInt32, UInt32), OSStatus> {
        let mut size = 0;
        let mut writable = 0;
        // SAFETY: self.raw is live; out-params are writable.
        let status = unsafe {
            audio_file_get_property_info_raw(self.raw.as_ptr(), property, &mut size, &mut writable)
        };
        if status == NO_ERR {
            Ok((size, writable))
        } else {
            Err(status)
        }
    }

    /// Opens an `AudioToolbox` file around Rust callbacks.
    ///
    /// # Errors
    ///
    /// Returns the `AudioToolbox` status from `audio_file_open_with_callbacks` or a
    /// parameter error if Apple returns a null handle.
    pub fn open_with_callbacks(
        callbacks: C,
        hint: Option<AudioFileTypeID>,
        include_size_callback: bool,
    ) -> Result<Self, OSStatus> {
        let mut callbacks = Box::new(callbacks);
        let mut handle: AudioFileID = ptr::null_mut();
        let get_size =
            include_size_callback.then_some(get_size_callback::<C> as extern "C" fn(_) -> _);
        // SAFETY: callbacks is boxed and outlives the AudioFile handle.
        // SAFETY: AudioFileServices callbacks are synchronous for that boxed state.
        let status = unsafe {
            audio_file_open_with_callbacks(
                callbacks.as_mut() as *mut C as *mut c_void,
                read_callback::<C>,
                ptr::null(),
                get_size,
                ptr::null(),
                hint.unwrap_or(0),
                &mut handle,
            )
        };
        if status != NO_ERR {
            return Err(status);
        }
        let raw = NonNull::new(handle).ok_or(PARAM_ERR)?;
        Ok(Self { raw, callbacks })
    }

    /// Reads compressed packet data from `AudioToolbox`.
    ///
    /// # Errors
    ///
    /// Returns the `AudioToolbox` status when packet data cannot be read.
    pub fn read_packet_data(
        &mut self,
        starting_packet: SInt64,
        packet_description: Option<&mut AudioStreamPacketDescription>,
        packets: &mut UInt32,
        buffer: &mut [u8],
    ) -> Result<AudioFilePacketRead, OSStatus> {
        let mut bytes = UInt32::try_from(buffer.len()).map_err(|_| PARAM_ERR)?;
        let packet_description = packet_description.map_or(ptr::null_mut(), ptr::from_mut);
        // SAFETY: self.raw is live; buffer and out-params are valid for this call.
        let status = unsafe {
            audio_file_read_packet_data(
                self.raw.as_ptr(),
                0,
                &mut bytes,
                packet_description,
                starting_packet,
                packets,
                buffer.as_mut_ptr().cast::<c_void>(),
            )
        };
        if status == NO_ERR {
            Ok(AudioFilePacketRead {
                bytes,
                packets: *packets,
            })
        } else {
            Err(status)
        }
    }
}

impl<C> Drop for AudioFile<C>
where
    C: AudioFileCallbacks,
{
    fn drop(&mut self) {
        // SAFETY: raw was returned by audio_file_open_with_callbacks and is closed once here.
        let _ = unsafe { audio_file_close(self.raw.as_ptr()) };
    }
}

extern "C" fn read_callback<C>(
    user_data: *mut c_void,
    position: SInt64,
    request: UInt32,
    buffer: *mut c_void,
    actual: *mut UInt32,
) -> OSStatus
where
    C: AudioFileCallbacks,
{
    if user_data.is_null() || buffer.is_null() || actual.is_null() {
        return PARAM_ERR;
    }
    let Ok(request) = usize::try_from(request) else {
        return PARAM_ERR;
    };

    // SAFETY: user_data is the boxed callback object supplied by open.
    // SAFETY: buffer and actual are AudioFileServices out-params checked above.
    let callbacks = unsafe { &mut *(user_data as *mut C) };
    // SAFETY: buffer is non-null and has at least request bytes for the callback.
    let slice = unsafe { slice::from_raw_parts_mut(buffer.cast::<u8>(), request) };
    // SAFETY: actual was checked non-null above.
    let actual = unsafe { &mut *actual };
    match callbacks.read_at(position, slice) {
        Ok(bytes) => {
            *actual = bytes;
            NO_ERR
        }
        Err(status) => {
            *actual = 0;
            status
        }
    }
}

extern "C" fn get_size_callback<C>(user_data: *mut c_void) -> SInt64
where
    C: AudioFileCallbacks,
{
    if user_data.is_null() {
        return 0;
    }
    // SAFETY: user_data is the boxed callback object supplied by open.
    let callbacks = unsafe { &*(user_data as *const C) };
    callbacks.size()
}
