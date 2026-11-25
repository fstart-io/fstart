//! The Root Complex Register Block of a CXL 1.1 (RCH) Downstream Port (CXL
//! 3.1 §8.2.1.1): 4 KiB of MMIO laid out as a PCI type-1 header, whose
//! MEMBAR0 holds the port's component registers and whose bus numbers
//! route the RCD's configuration cycles.

use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use tock_registers::interfaces::Writeable;
use tock_registers::register_structs;

use crate::config::PCI_BRIDGE_BUSES;

/// Bytes of one RCRB (the downstream port's; the upstream port's follows).
pub const SIZE: u64 = 0x1000;

register_structs! {
    /// The head of a Downstream Port RCRB, dword accesses only.
    pub DownstreamPort {
        (0x00 => pub id: MmioReadOnly<u32>),
        (0x04 => pub command_status: MmioReadWrite<u32>),
        (0x08 => _reserved0),
        /// MEMBAR0, low and high dword (a 64-bit memory BAR).
        (0x10 => pub membar0_low: MmioReadWrite<u32>),
        (0x14 => pub membar0_high: MmioReadWrite<u32>),
        (0x18 => pub buses: MmioReadWrite<u32, PCI_BRIDGE_BUSES::Register>),
        (0x1c => @END),
    }
}

impl DownstreamPort {
    /// Place MEMBAR0 at `base`.
    pub fn set_membar0(&self, base: u64) {
        self.membar0_low.set(base as u32);
        self.membar0_high.set((base >> 32) as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tock_registers::interfaces::{ReadWriteable, Readable};

    #[test]
    fn the_rcrb_is_a_type1_header_head() {
        let mut ram = [0u32; 8];
        ram[6] = 0x4000_0000;
        // SAFETY: a RAM stand-in of the RCRB's head.
        let rcrb = unsafe { &*(ram.as_mut_ptr() as *const DownstreamPort) };
        rcrb.set_membar0(0x12_3456_0000);
        rcrb.buses.modify(
            PCI_BRIDGE_BUSES::PRIMARY.val(0)
                + PCI_BRIDGE_BUSES::SECONDARY.val(0x42)
                + PCI_BRIDGE_BUSES::SUBORDINATE.val(0x42),
        );
        assert_eq!(rcrb.buses.get(), 0x4042_4200);
        assert_eq!(ram[4..6], [0x3456_0000, 0x12]);
    }
}
