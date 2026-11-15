// Extracted and adapted by Kithara from Apache-2.0 wreq-util 3.0.0-rc.14.

use std::io::{self, Write};

use brotli::{CompressorWriter as BrotliEncoder, Decompressor as BrotliDecoder};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use wreq::tls::compress::{CertificateCompressionAlgorithm, CertificateCompressor, Codec};

#[derive(Debug)]
pub(super) struct BrotliCompressor;

#[derive(Debug)]
pub(super) struct ZlibCompressor;

pub(super) mod consts {
    use super::{BrotliCompressor, CertificateCompressor, ZlibCompressor};

    pub(crate) const CHROME_CERTIFICATE_COMPRESSORS: &[&'static dyn CertificateCompressor] =
        &[&BrotliCompressor];
    pub(crate) const SAFARI_CERTIFICATE_COMPRESSORS: &[&'static dyn CertificateCompressor] =
        &[&ZlibCompressor];
}

impl CertificateCompressor for BrotliCompressor {
    fn compress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut writer = BrotliEncoder::new(output, input.len(), 11, 32);
            writer.write_all(input)?;
            writer.flush()?;
            Ok(())
        })
    }

    fn decompress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut reader = BrotliDecoder::new(input, 4096);
            io::copy(&mut reader, output)?;
            Ok(())
        })
    }

    fn algorithm(&self) -> CertificateCompressionAlgorithm {
        CertificateCompressionAlgorithm::BROTLI
    }
}

impl CertificateCompressor for ZlibCompressor {
    fn compress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut encoder = ZlibEncoder::new(output, Compression::default());
            encoder.write_all(input)?;
            encoder.finish()?;
            Ok(())
        })
    }

    fn decompress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut reader = ZlibDecoder::new(input);
            io::copy(&mut reader, output)?;
            Ok(())
        })
    }

    fn algorithm(&self) -> CertificateCompressionAlgorithm {
        CertificateCompressionAlgorithm::ZLIB
    }
}
