//! PSP BAR and `PSP_ADDR` (PSP_APOB §3.2).
//!
//! The BAR is IOHC `PSP_BASE_ADDR_LO/HI` (SMN `0x13b102e0/e4`), 1 MiB,
//! programmed `base | enable | lock`. AGESA puts it in the non-PCI MMIO
//! window that the DF routes to the IOMS of that IOHC (RB 0), skips sockets
//! whose BAR is already set, and writes the BAR into MSR `C001_10A2`
//! (`PSP_ADDR`) on every thread of the socket (Rome, Milan). The address
//! comes from the caller (the DF/NBIO MMIO planner).
//!
//! | | Naples | Rome | Milan |
//! |---|---|---|---|
//! | BAR | per die (AGESA 1.0.0.B: die 0 only) | per socket, IOHC0 | per socket, IOHC0 |
//! | `PSP_ADDR` | BSP only in AGESA | every thread | every thread |
//!
//! fstart programs every die on Naples and the MSR on every thread on all
//! three: a superset of AGESA (PSP_APOB §5).

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

use crate::smn::{Smn, SmnAddr};
use crate::soc::Generation;

/// IOHC0 `PSP_BASE_ADDR_LO`.
pub const PSP_BASE_ADDR_LO: SmnAddr = 0x13b1_02e0;
/// IOHC0 `PSP_BASE_ADDR_HI`.
pub const PSP_BASE_ADDR_HI: SmnAddr = 0x13b1_02e4;
/// BAR size and alignment.
pub const BAR_SIZE: u64 = 1 << 20;
/// `PSP_ADDR`: the BAR as seen from each thread.
pub const MSR_PSP_ADDR: u32 = 0xc001_10a2;

register_bitfields![u32,
    pub PSP_BASE_LO [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(8) NUMBITS(1) [],
        /// Address bits 31:20.
        ADDR OFFSET(20) NUMBITS(12) [],
    ]
];

/// How many BARs a generation has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// One per ZP die (Naples).
    Die,
    /// One per IOD (Rome, Milan).
    Socket,
}

pub const fn scope(generation: Generation) -> Scope {
    match generation {
        Generation::Naples => Scope::Die,
        Generation::Rome | Generation::Milan => Scope::Socket,
    }
}

/// Index of the BAR serving a thread on `(socket, die)`, for a per-system
/// table of `sockets * dies_per_socket` (Naples) or `sockets` entries.
pub const fn index(generation: Generation, socket: u8, die: u8) -> usize {
    match scope(generation) {
        Scope::Die => socket as usize * generation.dies_per_socket() + die as usize,
        Scope::Socket => socket as usize,
    }
}

/// BAR programming errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarError {
    /// Zero or not 1 MiB aligned.
    Misaligned(u64),
    /// The write did not stick (lock or wrong SMN target).
    Readback { wanted: u64, read: u64 },
}

/// The current BAR, `None` if unset (AGESA: `SMN & 0xfff00000`, zero
/// meaning unset).
///
/// # Safety
/// As [`Smn::read32`]; `smn` reaches the IOHC of the socket (Rome, Milan)
/// or die (Naples).
pub unsafe fn read<P: ConfigRegionAccess + Clone>(smn: &Smn<P>) -> Option<u64> {
    // SAFETY: the caller's contract.
    let (lo, hi) = unsafe {
        (
            smn.register::<PSP_BASE_LO::Register>(PSP_BASE_ADDR_LO),
            smn.register::<()>(PSP_BASE_ADDR_HI),
        )
    };
    let base = u64::from(hi.get()) << 32 | u64::from(lo.read(PSP_BASE_LO::ADDR)) << 20;
    (base != 0).then_some(base)
}

/// Program the BAR to `base` unless it is already set, as AGESA does, and
/// return the BAR in effect.
///
/// HI is written before LO because LO carries the lock (order not visible
/// in AGESA's SMN helper; inference).
///
/// # Safety
/// As [`read`]; `base` is a free 1 MiB of MMIO routed to this IOHC.
pub unsafe fn program<P: ConfigRegionAccess + Clone>(
    smn: &Smn<P>,
    base: u64,
) -> Result<u64, BarError> {
    if base == 0 || !base.is_multiple_of(BAR_SIZE) {
        return Err(BarError::Misaligned(base));
    }
    // SAFETY: the caller's contract.
    unsafe {
        if let Some(current) = read(smn) {
            return Ok(current);
        }
        smn.register::<()>(PSP_BASE_ADDR_HI)
            .set((base >> 32) as u32);
        smn.register::<PSP_BASE_LO::Register>(PSP_BASE_ADDR_LO)
            .write(
                PSP_BASE_LO::ADDR.val((base >> 20) as u32 & 0xfff)
                    + PSP_BASE_LO::ENABLE::SET
                    + PSP_BASE_LO::LOCK::SET,
            );
        match read(smn) {
            Some(read) if read == base => Ok(base),
            other => Err(BarError::Readback {
                wanted: base,
                read: other.unwrap_or(0),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::model::Model;

    #[test]
    fn program_and_skip() {
        let model = Model::default();
        let smn = Smn::new(model.clone(), 0x40);
        unsafe {
            assert_eq!(read(&smn), None);
            assert_eq!(program(&smn, 0x1_f000_0000), Ok(0x1_f000_0000));
            assert_eq!(model.0.borrow().smn[&(0x40, PSP_BASE_ADDR_LO)], 0xf000_0101);
            assert_eq!(model.0.borrow().smn[&(0x40, PSP_BASE_ADDR_HI)], 1);
            // Already set: kept, as AGESA's skip rule.
            assert_eq!(program(&smn, 0xc000_0000), Ok(0x1_f000_0000));
            assert_eq!(
                program(&smn, 0xc008_0000),
                Err(BarError::Misaligned(0xc008_0000))
            );
        }
    }

    #[test]
    fn bar_index_per_generation() {
        assert_eq!(index(Generation::Naples, 1, 2), 6);
        assert_eq!(index(Generation::Rome, 1, 0), 1);
        assert_eq!(index(Generation::Milan, 0, 0), 0);
    }
}
