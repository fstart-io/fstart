//! Writers for a [`Plan`](super::plan::Plan): the MMIO and I/O rules and
//! the VGA route, on every DF node (SOC_DF §3.4.4, DF_ZP §3.10–§3.11).
//!
//! Every node gets the same routing: AGESA writes each rule on every node
//! (IC), base, then limit, then control (`ffe204c5`), and the ABL leaves
//! no rule that the plan must clear.

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Writeable};

use super::plan::{IoRoute, MmioRoute};
use super::regs::{self, IO_BASE};
use super::{Df, Version};

/// Program MMIO rules on every node in `nodes`. Bases are 64 KiB aligned
/// up, limits inclusive.
pub fn mmio<V: Version, P: ConfigRegionAccess>(nodes: &[Df<P>], routes: &[MmioRoute]) {
    for r in routes {
        let base = r.window.base.next_multiple_of(0x1_0000);
        let limit = base + r.window.size - 1;
        for df in nodes {
            df.reg(regs::MMIO_BASE.at(r.pair, regs::MMIO_STRIDE))
                .set((base >> 16) as u32);
            df.reg(regs::MMIO_LIMIT.at(r.pair, regs::MMIO_STRIDE))
                .set((limit >> 16) as u32);
            df.reg(V::mmio_ctrl_reg(r.pair))
                .write(V::mmio_ctrl(r.fabric_id));
        }
    }
}

/// Program I/O rules on every node in `nodes`, base before limit, as the
/// SoC writer does (`ffe205ee`, IC; Oxide writes the limit first).
pub fn io<V: Version, P: ConfigRegionAccess>(nodes: &[Df<P>], routes: &[IoRoute]) {
    for r in routes {
        let last = (r.window.end() - 1) as u32;
        for df in nodes {
            df.reg(regs::IO_BASE.at(r.pair, regs::IO_STRIDE)).write(
                IO_BASE::RE::SET
                    + IO_BASE::WE::SET
                    + IO_BASE::BASE.val((r.window.base >> 12) as u32),
            );
            df.reg(V::io_limit_reg(r.pair))
                .write(V::io_limit(last, r.fabric_id));
        }
    }
}

/// Route the legacy VGA window (0xA0000–0xBFFFF, VGA ports) to `fabric_id`
/// on every node in `nodes` (AmdFabricSspDxe `0x700c`; DF_ZP §3.11).
pub fn vga<V: Version, P: ConfigRegionAccess>(nodes: &[Df<P>], fabric_id: u16) {
    for df in nodes {
        df.reg(V::vga_reg()).modify(V::vga(fabric_id));
    }
}
