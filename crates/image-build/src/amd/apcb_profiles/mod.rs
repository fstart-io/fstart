// SPDX-License-Identifier: MIT OR Apache-2.0
//! Firmware-family models and reviewed defaults. No user-visible template DSL.
use super::legacy::{ParameterLayer, Setting, Width};
use super::rows::native;
use super::*;
mod picasso;
mod sp3;
mod zeppelin;

/// A reviewed firmware baseline, normally selected by the board build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    S8030RomeV404,
    S8030MilanV404,
    EthanolxMilanRxm100cb,
    Mj11Ec0F02,
    Mj11Ec1F09,
    Vilboz,
}
impl Profile {
    pub(crate) fn definition(self) -> Result<Definition> {
        match self {
            Self::S8030RomeV404 => sp3::s8030_rome(),
            Self::S8030MilanV404 => sp3::s8030_milan(),
            Self::EthanolxMilanRxm100cb => sp3::ethanolx_milan(),
            Self::Mj11Ec0F02 => zeppelin::mj11(true),
            Self::Mj11Ec1F09 => zeppelin::mj11(false),
            Self::Vilboz => picasso::vilboz(),
        }
    }
}
impl std::str::FromStr for Profile {
    type Err = String;
    fn from_str(name: &str) -> Result<Self> {
        Ok(match name {
            "s8030-rome-v404" => Self::S8030RomeV404,
            "s8030-milan-v404" => Self::S8030MilanV404,
            "ethanolx-milan-rxm100cb" => Self::EthanolxMilanRxm100cb,
            "mj11-ec0-f02" => Self::Mj11Ec0F02,
            "mj11-ec1-f09" => Self::Mj11Ec1F09,
            "vilboz" => Self::Vilboz,
            _ => return Err(format!("unknown APCB firmware profile {name:?}")),
        })
    }
}
