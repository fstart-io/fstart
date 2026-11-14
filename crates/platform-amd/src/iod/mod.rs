// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SoCs of an I/O die and CCDs in the SP3 socket, Rome
//! ([`crate::rome`]) and Milan ([`crate::milan`]): DF3, the NBIO
//! quadrants, SMU V11, two-pass DXIO, Huashan FCHs and the per-CCD SMU
//! thread release ([`silicon`]), on the flow with the socket's services
//! ([`crate::sp3::soc`]). The two differ in their die's IP versions
//! ([`Variant`]).
#[cfg(all(feature = "bundle", target_os = "none"))]
mod init;
pub mod silicon;

use core::marker::PhantomData;

/// The die of an IOD SoC: what Rome and Milan differ in.
pub trait Variant: 'static {
    /// The log prefix.
    const NAME: &'static str;
    const SILICON: fstart_driver_amd::cpu::Silicon;
}

/// An IOD SoC of the SP3 socket: [`crate::rome::Rome`],
/// [`crate::milan::Milan`].
pub struct Iod<V>(PhantomData<V>);

/// What the IOD SoCs' silicon steps hand on.
#[cfg(all(feature = "bundle", target_os = "none"))]
pub type State = crate::sp3::soc::State<silicon::Silicon>;
