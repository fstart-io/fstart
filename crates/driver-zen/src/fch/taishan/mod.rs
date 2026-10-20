//! Taishan (Naples ZP): one FCH per die with its own SATA controller, USB
//! controller and xGbE ports, programmed by the x86 over that die's SMN
//! (FCH_SP3 §3.1 af47/af48, §3.2 af49/af57, §4.1).
//!
//! Every function takes the `FchHw` of the die it programs; its
//! [`FchHw::bus`](super::FchHw::bus) selects the die's SMN.

pub mod sata;
pub mod usb;
pub mod xgbe;
