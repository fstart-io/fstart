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
        (0x1c => _reserved1),
        /// Memory base and limit (type-1 header `+0x20`).
        (0x20 => pub memory: MmioReadWrite<u32>),
        /// Prefetchable memory base and limit, and their upper dwords.
        (0x24 => pub prefetch: MmioReadWrite<u32>),
        (0x28 => pub prefetch_base_high: MmioReadWrite<u32>),
        (0x2c => pub prefetch_limit_high: MmioReadWrite<u32>),
        (0x30 => @END),
    }
}

register_structs! {
    /// The head of an Upstream Port RCRB (a type-0 header), dword accesses
    /// only: BAR0 holds the upstream port's component registers.
    pub UpstreamPort {
        (0x00 => pub id: MmioReadOnly<u32>),
        (0x04 => pub command_status: MmioReadWrite<u32>),
        (0x08 => _reserved0),
        (0x10 => pub bar0_low: MmioReadWrite<u32>),
        (0x14 => pub bar0_high: MmioReadWrite<u32>),
        (0x18 => @END),
    }
}

/// The memory base/limit dword of a type-1 header forwarding `base..base +
/// size` (1 MiB granular): base bits 31:20 in 15:4, limit bits 31:20 in
/// 31:20.
pub const fn memory_window(base: u64, size: u64) -> u32 {
    let limit = base + size - 1;
    ((base >> 16) as u32 & 0xfff0) | (limit as u32 & 0xfff0_0000)
}

impl UpstreamPort {
    /// Place BAR0 at `base`.
    pub fn set_bar0(&self, base: u64) {
        self.bar0_low.set(base as u32);
        self.bar0_high.set((base >> 32) as u32);
    }
}

impl DownstreamPort {
    /// Place MEMBAR0 at `base`.
    pub fn set_membar0(&self, base: u64) {
        self.membar0_low.set(base as u32);
        self.membar0_high.set((base >> 32) as u32);
    }

    /// Forward `memory` (below 4 GiB) and the 64-bit prefetchable
    /// `prefetch` window, `(base, size)` each, to the device (AmdNbioDxe
    /// `0x357B0`: `+0x24 | 0x10001`, the 64-bit type in both halves).
    pub fn set_windows(&self, memory: (u64, u64), prefetch: (u64, u64)) {
        self.memory.set(memory_window(memory.0, memory.1));
        self.prefetch
            .set(memory_window(prefetch.0, prefetch.1) | 0x1_0001);
        self.prefetch_base_high.set((prefetch.0 >> 32) as u32);
        self.prefetch_limit_high
            .set(((prefetch.0 + prefetch.1 - 1) >> 32) as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tock_registers::interfaces::{ReadWriteable, Readable};

    #[test]
    fn the_rcrb_is_a_type1_header_head() {
        let mut ram = [0u32; 12];
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
        rcrb.set_windows((0xe000_0000, 0x10_0000), (0x40_0000_0000, 0x1_0000_0000));
        assert_eq!(ram[8], 0xe000_e000);
        assert_eq!(ram[9], 0xfff0_0000 | 0x1_0001);
        assert_eq!(ram[10..12], [0x40, 0x40]);
    }
}
