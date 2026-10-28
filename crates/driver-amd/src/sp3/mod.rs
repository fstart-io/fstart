//! AMD Zen server silicon for the SP3 socket: Naples (ZP), Rome (SSP) and
//! Milan (GN), one and two sockets.
//!
//! The IP blocks are split by block and, where the IP differs, by version:
//!
//! | Block | Naples | Rome | Milan |
//! |---|---|---|---|
//! | [`df`] | DF2 | DF3 | DF3 |
//! | [`nbio`] | ZP NBIO (1 per die) | SSP NBIO (4 per IOD) | GN NBIO (fuse filtered) |
//! | [`smu`] | V9 | V11 | V11 |
//! | [`dxio`] | per die | per socket, shared SP3 lane map | as Rome |
//! | [`ccx`] | Zen | Zen 2 | Zen 3 |
//! | [`fch`] | Taishan | Huashan | Huashan |
//! | [`psp`], [`apob`] | shared transport, generation rules | | |
//!
//! Everything here is written from the SP3 reverse engineering reports and
//! public AMD documents; register names follow the PPRs where public.
//! Unknown CPUs fail closed in [`soc::identify`].

pub mod apob;
pub mod ccx;
pub mod df;
pub mod dxio;
pub mod fch;
pub mod nbio;
pub mod psp_bar;
pub mod soc;
pub mod topology;
