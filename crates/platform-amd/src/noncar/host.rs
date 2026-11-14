// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host build of the SoCs made of dies: their [`crate::host::Host`]
//! data, from the SoC's layout and image ([`HostSoc`]).
extern crate std;
use super::DieSoc;
use crate::host::{Host, single};

/// What a die SoC's host build differs in.
pub trait HostSoc: DieSoc {
    /// Debug builds don't fit the image.
    const RELEASE_ONLY: bool;
    /// Where the PSP firmware of a full image comes from.
    const SOURCE: single::Source;
}

/// The host data of a board with the die SoC `S` and up to `max_cpus`
/// CPUs.
pub fn host<S: HostSoc>(max_cpus: u16) -> Host {
    Host {
        name: <S as crate::soc::Init>::NAME,
        layout: S::LAYOUT,
        max_cpus,
        tseg_size: super::TSEG_SIZE.into(),
        release_only: S::RELEASE_ONLY,
        i2c_input: true,
        image: crate::host::Image::Single(single::Image {
            soc: <S as crate::soc::Init>::NAME,
            bios_directory: S::BIOS_DIRECTORY,
            apob_destination: S::APOB.base,
            source: S::SOURCE,
        }),
    }
}

#[cfg(test)]
mod tests;
