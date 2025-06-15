use embedded_io_async::{Read, ErrorKind, ErrorType};
use digest::{Update, OutputSizeUser, FixedOutputReset};
use signature::Verifier;
use crate::{Error, Result};
use super::{SignatureVerify, HashVerify};

pub struct VerifiedFullRead<'a, R, V>
{
    dest:   &'a mut [u8],
    offset: usize,
    source: R,
    verify: V,
    result: Option<Result<()>>,
}

impl<'a, R, V> VerifiedFullRead<'a, R, V> where Self: ReadVerify {
    pub const fn new(dest: &'a mut [u8], src: R, verify: V) -> Self {
        Self {
            dest:   dest,
            offset: 0,
            source: src,
            verify: verify,
            result: None,
        }
    }

    fn clear_invalid(&mut self) -> Error {
        self.dest.fill(0);
        Error::InvalidData
    }

    pub async fn read_and_verify(mut self) -> Result<()> {
        <Self as ReadVerify>::read_and_verify(&mut self).await
    }
}

pub trait ReadVerify {
    #[allow(async_fn_in_trait)]
    async fn read_and_verify(&mut self) -> Result<()>;
}

impl<R, V, S> ReadVerify for VerifiedFullRead<'_, R, SignatureVerify<'_, V, S>>
where
    R: Read + ErrorType<Error = Error>,
    V: Verifier<S>,
{
    async fn read_and_verify(&mut self) -> Result<()> {
        if self.result.is_none() {
            self.result = Some(
                self.source.read_exact(&mut self.dest).await
                .map_err(crate::rex_to_error)
                .and_then(|_| self.verify.verify(&self.dest)
                    .map_err(|_| self.clear_invalid())));
        }
        self.result.unwrap()
    }
}

impl<R, D> ReadVerify for VerifiedFullRead<'_, R, HashVerify<'_, D>>
where
    R: Read + ErrorType<Error = Error>,
    D: Update + OutputSizeUser + FixedOutputReset,
{
    async fn read_and_verify(&mut self) -> Result<()> {
        while self.result.is_none() {
            match self.source.read(&mut self.dest[self.offset..]).await? {
                0 => {
                    self.result = Some(
                        if self.offset != self.dest.len() {
                            Err(ErrorKind::BrokenPipe)
                        } else {
                            self.verify.verify().map_err(|_| self.clear_invalid())
                        }
                    );
                }
                len => {
                    self.verify.update(&self.dest[self.offset..][..len]);
                    self.offset += len;
                }
            }
        }
        self.result.unwrap()
    }
}
