// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host build of the SoCs made of dies: their [`crate::host::Host`]
//! data, from the SoC's layout and image ([`HostSoc`]) and the board.
extern crate std;
use super::{Board, DieSoc};
use crate::host::{self, Host, single};
use fstart_image_build::{
    build_plan::BuildPlan,
    plan::{BuildSelection, HostPlan, ResolvedPlan},
};
use std::string::String;

/// What a die SoC's host build differs in.
pub trait HostSoc: DieSoc {
    /// Debug builds don't fit the image.
    const RELEASE_ONLY: bool;
    /// Where the PSP firmware of a full image comes from.
    const SOURCE: single::Source;
    /// Check a board's configuration before building.
    fn check(_config: &Self::Config) -> Result<(), &'static str> {
        Ok(())
    }
}

/// The host data of board `B`.
fn host<B: Board>() -> Host<'static>
where
    B::Soc: HostSoc,
{
    Host {
        name: <B::Soc as crate::soc::Init>::NAME,
        layout: <B::Soc as DieSoc>::LAYOUT,
        max_cpus: <B::Soc as DieSoc>::MAX_CPUS,
        tseg_size: super::TSEG_SIZE.into(),
        release_only: <B::Soc as HostSoc>::RELEASE_ONLY,
        i2c_input: true,
        image: host::Image::Single(single::Image {
            soc: <B::Soc as crate::soc::Init>::NAME,
            bios_directory: <B::Soc as DieSoc>::BIOS_DIRECTORY,
            apob_destination: <B::Soc as DieSoc>::APOB.base,
            source: <B::Soc as HostSoc>::SOURCE,
        }),
    }
}

/// The board's plan generator: `fstart_platform_amd::Plan`.
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: Board> HostPlan<B> for Plan<B>
where
    B::Soc: HostSoc,
{
    type Options = crate::HostOptions;

    fn resolve(selection: BuildSelection, options: Self::Options) -> Result<ResolvedPlan, String> {
        resolve::<B>(selection, &options)
    }
}

/// The build plan of board `B`.
pub fn resolve<B: Board>(
    selection: BuildSelection,
    options: &crate::HostOptions,
) -> Result<BuildPlan, String>
where
    B::Soc: HostSoc,
{
    <B::Soc as HostSoc>::check(B::CONFIG)?;
    host::resolve(&host::<B>(), &selection, options)
}

#[cfg(test)]
mod tests;
