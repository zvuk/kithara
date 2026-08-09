mod bytes;
mod packaged;
#[cfg(all(test, not(feature = "ffmpeg")))]
mod tests;

pub(crate) use self::packaged::OfflineEncoder;
