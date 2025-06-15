/*++

Licensed under the Apache-2.0 license.

File Name:

lib.rs

Abstract:

File contains exports for fstart Library.

--*/

pub type Error = embedded_io_async::ErrorKind;
pub type Result<T> = core::result::Result<T, Error>;
