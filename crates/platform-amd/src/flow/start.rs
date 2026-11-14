// SPDX-License-Identifier: MIT OR Apache-2.0
//! Which SoC this is, before anything else: CPUID picks the SoC whose flow
//! the board's `main` runs ([`start`]). One reset image carries every SoC
//! the board lists; it calls the flow of each and
//! [`Identified::unsupported`] for the others, so only the flows it calls
//! are linked.
use core::marker::PhantomData;

use super::{Early, Soc};
use crate::board::{Board, Facts, SocId};

/// The running SoC: what [`start`] returns.
pub enum Cpu<B> {
    Picasso(Identified<B, crate::picasso::Picasso>),
    Zeppelin(Identified<B, crate::zeppelin::Zeppelin>),
    Naples(Identified<B, crate::naples::Naples>),
    Rome(Identified<B, crate::rome::Rome>),
    Milan(Identified<B, crate::milan::Milan>),
}

impl<B> Cpu<B> {
    /// A SoC the board's flow does not handle: halt at POST 13, before the
    /// console (its code is not linked).
    pub fn unsupported(self) -> ! {
        unsupported()
    }
}

/// The SoC is identified; the board enters its flow with its configuration
/// of it next ([`Identified::enter`]).
pub struct Identified<B, S>(PhantomData<(B, S)>);

impl<B: Board, S: Soc> Identified<B, S> {
    /// The first phase of the SoC's flow ([`super::enter`]) with the
    /// board's `config` of it: ECAM, the SoC's handles and its FCH setup
    /// for the console.
    pub fn enter(self, config: S::Config) -> Early<S> {
        super::enter(Facts::of::<B>(), config)
    }

    /// A SoC the board's flow does not handle: halt at POST 13.
    pub fn unsupported(self) -> ! {
        unsupported()
    }
}

fn unsupported() -> ! {
    fstart_driver_amd::fch::post_native(0x13);
    fstart_arch::x86::boot::halt()
}

/// The SoC of CPUID Fn0000_0001_EAX `signature` and the package type of
/// Fn8000_0001_EBX\[31:28\].
const fn soc(signature: u32, package: u8) -> Option<SocId> {
    use fstart_driver_amd::cpu::{Revision, Silicon};
    if let Ok(revision) = Revision::from_signature(signature) {
        return Some(match revision.silicon() {
            Silicon::Zp if package == crate::sp3::silicon::PACKAGE_SP3 => SocId::Naples,
            Silicon::Zp => SocId::Zeppelin,
            Silicon::Ssp => SocId::Rome,
            Silicon::Gn => SocId::Milan,
        });
    }
    // Raven, Picasso, Raven2 and Dali: Family 17h models 10h–2Fh.
    let family = ((signature >> 8) & 0xf) + ((signature >> 20) & 0xff);
    let model = ((signature >> 12) & 0xf0) | ((signature >> 4) & 0xf);
    if family == 0x17 && model >= 0x10 && model <= 0x2f {
        return Some(SocId::Picasso);
    }
    None
}

/// Identify the SoC. One the board does not list, or no SoC the platform
/// knows, halts at POST 13.
pub fn start<B: Board>() -> Cpu<B> {
    let signature = fstart_arch::x86::cpuid(1).0;
    let package = (fstart_arch::x86::cpuid(0x8000_0001).1 >> 28) as u8;
    let soc = match soc(signature, package) {
        Some(soc) if B::SOCS.contains(&soc) => soc,
        _ => unsupported(),
    };
    match soc {
        SocId::Picasso => Cpu::Picasso(Identified(PhantomData)),
        SocId::Zeppelin => Cpu::Zeppelin(Identified(PhantomData)),
        SocId::Naples => Cpu::Naples(Identified(PhantomData)),
        SocId::Rome => Cpu::Rome(Identified(PhantomData)),
        SocId::Milan => Cpu::Milan(Identified(PhantomData)),
    }
}
