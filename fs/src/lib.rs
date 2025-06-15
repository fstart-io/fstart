/*++

Licensed under the Apache-2.0 license.

File Name:

lib.rs

Abstract:

File contains exports for fstart Library.

--*/

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
mod test;

pub mod config;
pub mod crypto;
pub mod metadata;

use zerocopy::AsBytes;
use embedded_io_async::{ErrorType, ErrorKind::{self, *}, ReadExactError,
                        Read, Seek, SeekFrom};
use fdt::Fdt;

use crate::crypto::{VerifiedFullRead, SignatureVerify};
use crate::metadata::DtfsHeader;

pub type Error = embedded_io_async::ErrorKind;
pub type Result<T> = core::result::Result<T, Error>;

const MAX_METADATA_SIZE: usize = 4096;

pub struct FileSystem<'a, F, V> {
    dtfs_buf:       [u8; MAX_METADATA_SIZE],
    verified_len:   usize,
    dtfs:           Option<Fdt<'a>>,
    flash:          F,
    verifier:       V,
}

impl<'a, F, V> FileSystem<'a, F, V>
where
    F: Read + Seek + ErrorType<Error = ErrorKind>,
{
    pub async fn load_fs<S>(&'a mut self, offset: u32) -> Result<()>
    where
        V: signature::Verifier<S> + crate::crypto::ParseSignature<S>,
    {
        let flash = &mut self.flash;
        let verifier = &self.verifier;

        let (dtfs_offset, sig_offset) =
            validate_header(flash, offset, self.dtfs_buf.len()).await?;
        let dtfs_len = (sig_offset - dtfs_offset) as usize;

        flash.seek(SeekFrom::Start(sig_offset)).await?;
        let signatures = V::try_parse_signature(flash).await
                                        .ok_or(InvalidData)?;

        flash.seek(SeekFrom::Start(dtfs_offset)).await?;
        let verify = SignatureVerify::new(verifier, &signatures);
        let verify = VerifiedFullRead::new(
            &mut self.dtfs_buf[..dtfs_len], flash, verify);
        self.verified_len = 0;
        verify.read_and_verify().await?;
        self.dtfs = Some(Fdt::new(&self.dtfs_buf[..dtfs_len])
                                        .or(Err(InvalidData))?);
        self.verified_len = dtfs_len;
        Ok(())
    }
}

// Returns absolute offsets of DTFS and signatures.
async fn validate_header<F>(flash: &mut F, offset: u32, max_size: usize)
    -> Result<(u64, u64)>
where
    F: Read + Seek + ErrorType<Error = ErrorKind>,
{
    let mut header = DtfsHeader::default();
    flash.seek(SeekFrom::Start(offset as u64)).await?;
    flash.read_exact(header.as_bytes_mut()).await.map_err(rex_to_error)?;
    let header = header;

    if (header.dtfs_offset as usize) < size_of::<DtfsHeader>()
        || header.magic != *DtfsHeader::DTFS_MAGIC
        || header.dtfs_offset > header.signatures_offset
        || offset + header.dtfs_offset < offset
        || offset + header.signatures_offset < offset
    {
        return Err(InvalidData);
    }

    if (header.signatures_offset - header.dtfs_offset) as usize > max_size {
        return Err(OutOfMemory);
    }

    Ok(((offset + header.dtfs_offset) as u64,
        (offset + header.signatures_offset) as u64))
}

pub(crate) fn rex_to_error(err: ReadExactError<Error>) -> Error {
    match err {
        ReadExactError::UnexpectedEof => BrokenPipe,
        ReadExactError::Other(err)    => err
    }
}
