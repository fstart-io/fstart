//! DDR3 mode-register value construction for Sandy Bridge native raminit.

use super::state::ControllerTopology;
use super::timing::TimingParams;

/// DDR3 mode register set for one rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModeRegisters {
    pub mr0: u16,
    pub mr1: u16,
    pub mr2: u16,
    pub mr3: u16,
}

/// Build MR0..MR3 values matching coreboot's Sandy Bridge native raminit policy.
pub fn build_mode_registers(
    timing: &TimingParams,
    topology: &ControllerTopology,
    channel: usize,
    auto_self_refresh: bool,
) -> ModeRegisters {
    let odt = odt_for_channel(topology.rankmap[channel]);
    ModeRegisters {
        mr0: make_mr0(timing),
        mr1: 2 | encode_odt(odt.rttnom),
        mr2: make_mr2(timing, odt.rttwr, auto_self_refresh),
        mr3: 0,
    }
}

fn make_mr0(timing: &TimingParams) -> u16 {
    const MCH_WR_T: [u16; 12] = [1, 2, 3, 4, 0, 5, 0, 6, 0, 7, 0, 0];

    let mch_cas = if timing.cas < 12 {
        u16::from(timing.cas - 4) << 1
    } else {
        (u16::from(timing.cas - 12) << 1) | 1
    };
    let wr_index = timing.twr.saturating_sub(5).min(11) as usize;
    let mch_wr = MCH_WR_T[wr_index];

    let mut mr0 = 1 << 8; // DLL reset; self-clearing after clock frequency change.
    mr0 |= (mch_cas & 0x1) << 2;
    mr0 |= (mch_cas & 0xe) << 3;
    mr0 |= mch_wr << 9;
    mr0 |= 1 << 12; // Precharge power-down fast exit for X220/Sandy first pass.
    mr0
}

fn make_mr2(timing: &TimingParams, rttwr_ohms: u16, auto_self_refresh: bool) -> u16 {
    let cwl = u16::from(timing.cwl.saturating_sub(5));
    let mut mr2 = cwl << 3;
    if auto_self_refresh {
        mr2 |= 1 << 6;
    }
    mr2 |= (rttwr_ohms / 60) << 9;
    mr2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OdtMap {
    rttwr: u16,
    rttnom: u16,
}

fn odt_for_channel(rankmap: u8) -> OdtMap {
    let dimms_per_ch = (rankmap & 1) + ((rankmap >> 2) & 1);
    if dimms_per_ch == 1 {
        OdtMap {
            rttwr: 60,
            rttnom: 60,
        }
    } else {
        OdtMap {
            rttwr: 120,
            rttnom: 30,
        }
    }
}

fn encode_odt(odt: u16) -> u16 {
    match odt {
        30 => (1 << 9) | (1 << 2),
        60 => 1 << 2,
        120 => 1 << 6,
        _ => 0,
    }
}

/// Apply DDR3 rank-1 address mirroring to an MRS bank/address pair.
pub fn mirror_mr_address(bank: u8, addr: u16) -> (u8, u16) {
    let mirrored_bank = ((bank >> 1) & 1) | ((bank << 1) & 2);
    let mirrored_addr = (addr & !0x01f8) | ((addr >> 1) & 0x00a8) | ((addr & 0x00a8) << 1);
    (mirrored_bank, mirrored_addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing() -> TimingParams {
        TimingParams {
            tck_256ns: 384,
            base_freq_mhz: 133,
            frq: 5,
            cas: 9,
            cwl: 7,
            trcd: 9,
            trp: 9,
            tras: 24,
            twr: 10,
            tfaw: 20,
            trrd: 4,
            trtp: 5,
            twtr: 5,
            trfc: 174,
            trefi: 5200,
            tmod: 12,
            txs_offset: 7,
            twlo: 6,
            tcke: 4,
            txpdll: 16,
            txp: 4,
            taonpd: 6,
            mdll_wake_delay: 336,
        }
    }

    #[test]
    fn builds_mode_registers_for_single_dimm_channel() {
        let topology = ControllerTopology {
            rankmap: [0b0011, 0],
            ..ControllerTopology::default()
        };
        let regs = build_mode_registers(&timing(), &topology, 0, false);
        assert_eq!(regs.mr0, 0x1b50);
        assert_eq!(regs.mr1, 0x0006);
        assert_eq!(regs.mr2, 0x0210);
        assert_eq!(regs.mr3, 0);
    }

    #[test]
    fn mirrors_mr_address_bits() {
        let (bank, addr) = mirror_mr_address(0b01, 0x00a8);
        assert_eq!(bank, 0b10);
        assert_eq!(addr & 0x0150, 0x0150);
    }
}
