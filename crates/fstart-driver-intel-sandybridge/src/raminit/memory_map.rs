//! Sandy Bridge host-bridge DRAM limit programming for native raminit.

use fstart_pio::{pci_cfg_read32, pci_cfg_write32};

use super::state::ControllerTopology;

const HOST_BUS: u8 = 0;
const HOST_DEV: u8 = 0;
const HOST_FUNC: u8 = 0;

const GGC: u8 = 0x50;
const MESEG_BASE: u8 = 0x70;
const MESEG_MASK: u8 = 0x78;
const REMAPBASE: u8 = 0x90;
const REMAPLIMIT: u8 = 0x98;
const TOM: u8 = 0xa0;
const TOUUD: u8 = 0xa8;
const BDSM: u8 = 0xb0;
const BGSM: u8 = 0xb4;
const TSEGMB: u8 = 0xb8;
const TOLUD: u8 = 0xbc;

const DEFAULT_PCI_MMIO_SIZE_MB: u32 = 2048;
const DEFAULT_TSEG_SIZE_MB: u32 = 8;
const ME_STLEN_EN: u32 = 1 << 11;
const MELCK: u32 = 1 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DramMemoryMap {
    pub tom_mb: u32,
    pub tolud_mb: u32,
    pub touud_mb: u32,
    pub tseg_base_mb: u32,
    pub gtt_base_mb: u32,
    pub gfx_stolen_base_mb: u32,
    pub remap_base_mb: Option<u32>,
    pub remap_limit_mb: Option<u32>,
    pub me_stolen_base_mb: Option<u32>,
}

pub fn program(topology: &ControllerTopology, me_uma_size_mb: u32) -> DramMemoryMap {
    let ggc = pci_read_host32(GGC) as u16;
    let (gfx_stolen_mb, gtt_size_mb) = igd_stolen_sizes_mb(ggc);
    let map = compute(
        topology,
        gfx_stolen_mb,
        gtt_size_mb,
        DEFAULT_TSEG_SIZE_MB,
        me_uma_size_mb,
    );

    write_64mb_limit(TOM, map.tom_mb);
    write_32mb_base(TOLUD, map.tolud_mb);
    write_64mb_limit(TOUUD, map.touud_mb);
    if let (Some(base), Some(limit)) = (map.remap_base_mb, map.remap_limit_mb) {
        write_split_limit(REMAPBASE, base);
        write_split_limit(REMAPLIMIT, limit);
    }
    write_32mb_base(TSEGMB, map.tseg_base_mb);
    write_32mb_base(BDSM, map.gfx_stolen_base_mb);
    write_32mb_base(BGSM, map.gtt_base_mb);
    if let Some(base) = map.me_stolen_base_mb {
        program_me_segment(base, me_uma_size_mb);
    }

    map
}

fn compute(
    topology: &ControllerTopology,
    gfx_stolen_mb: u32,
    gtt_size_mb: u32,
    tseg_size_mb: u32,
    me_uma_size_mb: u32,
) -> DramMemoryMap {
    let tom_mb = topology.channel_size_mb[0] + topology.channel_size_mb[1];
    let usable_tom_mb = tom_mb.saturating_sub(me_uma_size_mb);
    let me_stolen_base_mb = (me_uma_size_mb != 0).then_some(usable_tom_mb);
    let low_window_mb = 4096u32
        .saturating_sub(DEFAULT_PCI_MMIO_SIZE_MB)
        .saturating_add(gfx_stolen_mb)
        .saturating_add(gtt_size_mb)
        .saturating_add(tseg_size_mb);
    let mut tolud_mb = low_window_mb.min(usable_tom_mb);
    let mut gfx_stolen_base_mb = tolud_mb.saturating_sub(gfx_stolen_mb);
    let mut gtt_base_mb = gfx_stolen_base_mb.saturating_sub(gtt_size_mb);
    let mut tseg_base_mb = gtt_base_mb.saturating_sub(tseg_size_mb);

    let tseg_delta = if tseg_size_mb != 0 {
        tseg_base_mb & (tseg_size_mb - 1)
    } else {
        0
    };
    tseg_base_mb = tseg_base_mb.saturating_sub(tseg_delta);
    gtt_base_mb = gtt_base_mb.saturating_sub(tseg_delta);
    gfx_stolen_base_mb = gfx_stolen_base_mb.saturating_sub(tseg_delta);
    tolud_mb = tolud_mb.saturating_sub(tseg_delta);

    let (remap_base_mb, remap_limit_mb, touud_mb) = if usable_tom_mb > tolud_mb {
        let remap_base = 4096u32.max(usable_tom_mb);
        let remap_limit = remap_base + 4096u32.min(usable_tom_mb) - tolud_mb - 1;
        (Some(remap_base), Some(remap_limit), remap_limit + 1)
    } else {
        (None, None, usable_tom_mb)
    };

    DramMemoryMap {
        tom_mb,
        tolud_mb,
        touud_mb,
        tseg_base_mb,
        gtt_base_mb,
        gfx_stolen_base_mb,
        remap_base_mb,
        remap_limit_mb,
        me_stolen_base_mb,
    }
}

fn igd_stolen_sizes_mb(ggc: u16) -> (u32, u32) {
    if (ggc & 2) != 0 {
        (0, 0)
    } else {
        (
            u32::from((ggc >> 3) & 0x1f) * 32,
            u32::from((ggc >> 8) & 0x3),
        )
    }
}

fn program_me_segment(base_mb: u32, size_mb: u32) {
    let mask = 0x80000u32.saturating_sub(size_mb);
    let mut high_mask = pci_read_host32(MESEG_MASK + 4);
    high_mask = (high_mask & !0x000f_ffff) | ((mask & 0xffff_f000) >> 12);
    pci_write_host32(MESEG_MASK + 4, high_mask);

    let mut low_base = pci_read_host32(MESEG_BASE);
    low_base = (low_base & !0xfff0_0000) | ((base_mb & 0x0fff) << 20);
    pci_write_host32(MESEG_BASE, low_base);

    let mut high_base = pci_read_host32(MESEG_BASE + 4);
    high_base = (high_base & !0x000f_ffff) | ((base_mb & 0xffff_f000) >> 12);
    pci_write_host32(MESEG_BASE + 4, high_base);

    let mut low_mask = pci_read_host32(MESEG_MASK);
    low_mask = (low_mask & !0xfff0_0000) | ((mask & 0x0fff) << 20);
    low_mask |= ME_STLEN_EN | MELCK;
    pci_write_host32(MESEG_MASK, low_mask);
}

fn write_32mb_base(reg: u8, mb: u32) {
    let mut val = pci_read_host32(reg);
    val = (val & !0xfff0_0000) | ((mb & 0x0fff) << 20);
    pci_write_host32(reg, val);
}

fn write_64mb_limit(reg: u8, mb: u32) {
    write_32mb_base(reg, mb);
    let mut high = pci_read_host32(reg + 4);
    high = (high & !0x000f_ffff) | ((mb & 0xffff_f000) >> 12);
    pci_write_host32(reg + 4, high);
}

fn write_split_limit(reg: u8, mb: u32) {
    pci_write_host32(reg, mb << 20);
    pci_write_host32(reg + 4, mb >> 12);
}

fn pci_read_host32(reg: u8) -> u32 {
    // SAFETY: Native raminit runs on x86 BSP and accesses host bridge config space.
    unsafe { pci_cfg_read32(HOST_BUS, HOST_DEV, HOST_FUNC, reg) }
}

fn pci_write_host32(reg: u8, val: u32) {
    // SAFETY: Native raminit runs on x86 BSP and accesses host bridge config space.
    unsafe { pci_cfg_write32(HOST_BUS, HOST_DEV, HOST_FUNC, reg, val) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_reclaimed_8g_x220_style_map() {
        let topology = ControllerTopology {
            channel_size_mb: [4096, 4096],
            ..ControllerTopology::default()
        };
        let map = compute(&topology, 32, 2, 8, 0);
        assert_eq!(map.tom_mb, 8192);
        assert_eq!(map.tolud_mb, 2090);
        assert_eq!(map.tseg_base_mb, 2048);
        assert_eq!(map.gtt_base_mb, 2056);
        assert_eq!(map.gfx_stolen_base_mb, 2058);
        assert_eq!(map.remap_base_mb, Some(8192));
        assert_eq!(map.touud_mb, 10198);
        assert_eq!(map.me_stolen_base_mb, None);
    }
}
