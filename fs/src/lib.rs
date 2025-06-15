/*++

Licensed under the Apache-2.0 license.

File Name:

lib.rs

Abstract:

File contains exports for fstart Library.

--*/

pub mod crypto;

use embedded_io_async::{ErrorKind::*, ReadExactError};

pub type Error = embedded_io_async::ErrorKind;
pub type Result<T> = core::result::Result<T, Error>;

pub(crate) fn rex_to_error(err: ReadExactError<Error>) -> Error {
    match err {
        ReadExactError::UnexpectedEof => BrokenPipe,
        ReadExactError::Other(err)    => err
    }
}
