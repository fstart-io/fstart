// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Naples (EPYC 7001) in the SP3 socket: up to four ZP dies per
//! socket, each with its DF2 node, NBIO, SMU V9 and Taishan FCH
//! ([`silicon`]), on the flow with the socket's services
//! ([`crate::sp3::soc`]).
#[cfg(all(feature = "bundle", target_os = "none"))]
mod init;
pub mod silicon;

/// The Naples SoC.
pub struct Naples;

/// What Naples' silicon steps hand on.
#[cfg(all(feature = "bundle", target_os = "none"))]
pub type State = crate::sp3::soc::State<silicon::Silicon>;
