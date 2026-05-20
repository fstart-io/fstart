//! Sandy Bridge native raminit controller state derived from SPD.

use fstart_spd::ddr3::Ddr3DimmInfo;

use super::{DimmSlot, NUM_CHANNELS, NUM_SLOTS};

const NUM_SLOTRANKS: usize = 4;

/// Per-channel/rank topology and address-decoder values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerTopology {
    pub rankmap: [u8; NUM_CHANNELS],
    pub rank_mirror: [[bool; NUM_SLOTRANKS]; NUM_CHANNELS],
    pub mad_dimm: [u32; NUM_CHANNELS],
    pub channel_size_mb: [u32; NUM_CHANNELS],
}

impl ControllerTopology {
    pub fn from_dimms(dimms: &[[DimmSlot; NUM_SLOTS]; NUM_CHANNELS]) -> Self {
        let mut topology = Self::default();
        for (channel, slots) in dimms.iter().enumerate() {
            let mut slot_infos: [Option<Ddr3DimmInfo>; NUM_SLOTS] = [None; NUM_SLOTS];
            for (slot, dimm) in slots.iter().enumerate() {
                let Some(info) = dimm.info else {
                    continue;
                };
                slot_infos[slot] = Some(info);
                topology.channel_size_mb[channel] += info.module_capacity_mb;

                for rank in 0..info.ranks.min(2) {
                    topology.rankmap[channel] |= 1 << (slot * 2 + usize::from(rank));
                }
                if info.rank1_mirrored && info.ranks >= 2 {
                    topology.rank_mirror[channel][slot * 2 + 1] = true;
                }
            }
            topology.mad_dimm[channel] = mad_dimm_for_channel(slot_infos);
        }
        topology
    }
}

impl Default for ControllerTopology {
    fn default() -> Self {
        Self {
            rankmap: [0; NUM_CHANNELS],
            rank_mirror: [[false; NUM_SLOTRANKS]; NUM_CHANNELS],
            mad_dimm: [0; NUM_CHANNELS],
            channel_size_mb: [0; NUM_CHANNELS],
        }
    }
}

fn mad_dimm_for_channel(slots: [Option<Ddr3DimmInfo>; NUM_SLOTS]) -> u32 {
    let (a_idx, dimm_a, dimm_b) = match (slots[0], slots[1]) {
        (Some(slot0), Some(slot1)) if slot1.module_capacity_mb > slot0.module_capacity_mb => {
            (1u32, Some(slot1), Some(slot0))
        }
        (slot0, slot1) => (0u32, slot0, slot1),
    };

    let mut reg = a_idx << 16;
    if let Some(dimm) = dimm_a {
        reg |= (dimm.module_capacity_mb / 256) & 0xff;
        reg |= u32::from(dimm.ranks.saturating_sub(1)) << 17;
        reg |= u32::from((dimm.device_width_bits / 8).saturating_sub(1)) << 19;
    }
    if let Some(dimm) = dimm_b {
        reg |= ((dimm.module_capacity_mb / 256) & 0xff) << 8;
        reg |= u32::from(dimm.ranks.saturating_sub(1)) << 18;
        reg |= u32::from((dimm.device_width_bits / 8).saturating_sub(1)) << 20;
    }

    if dimm_a.is_some() || dimm_b.is_some() {
        // Coreboot enables rank interleave and enhanced interleave by default.
        reg | (1 << 21) | (1 << 22)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dimm(size_mb: u32, ranks: u8, mirrored: bool) -> DimmSlot {
        DimmSlot {
            present: true,
            spd_addr: 0x50,
            info: Some(Ddr3DimmInfo {
                ranks,
                rows: 15,
                cols: 10,
                banks: 8,
                device_width_bits: 8,
                bus_width_bits: 64,
                rank_capacity_mb: size_mb / u32::from(ranks),
                module_capacity_mb: size_mb,
                cas_latencies: 0x01fe,
                tck_min_ps: 1500,
                taa_min_ps: 13_500,
                twr_min_ps: 15_000,
                trcd_min_ps: 13_500,
                trrd_min_ps: 6_000,
                trp_min_ps: 13_500,
                tras_min_ps: 36_000,
                trc_min_ps: 49_500,
                trfc_min_ps: 260_000,
                twtr_min_ps: 7_500,
                trtp_min_ps: 7_500,
                tfaw_min_ps: 30_000,
                supports_1_5v: true,
                supports_auto_self_refresh: true,
                rank1_mirrored: mirrored,
            }),
        }
    }

    #[test]
    fn derives_x220_two_slot_topology() {
        let dimms = [
            [dimm(4096, 2, true), DimmSlot::default()],
            [dimm(4096, 2, true), DimmSlot::default()],
        ];
        let topology = ControllerTopology::from_dimms(&dimms);
        assert_eq!(topology.rankmap, [0b0011, 0b0011]);
        assert_eq!(topology.channel_size_mb, [4096, 4096]);
        assert!(topology.rank_mirror[0][1]);
        assert_eq!(topology.mad_dimm[0] & 0xff, 16);
        assert_ne!(topology.mad_dimm[0] & (1 << 21), 0);
        assert_ne!(topology.mad_dimm[0] & (1 << 22), 0);
    }
}
