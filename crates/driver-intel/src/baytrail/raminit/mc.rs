//! Memory controller (D-unit) and arbiter (B-unit) programming: timings,
//! the pre-JEDEC setup, the JEDEC init sequence, and the final geometry,
//! memory map and power-management configuration.

use super::dunit::*;
use super::{DdrFreq, Params};
use crate::baytrail::bunit::{self, BC0AHASHCFG, BISOC, BMBOUND, BMBOUND_HI, BMISC, BTHCTRL, SMRR};
use crate::baytrail::iosf::{DUNIT_CH0, FUSE, Iosf, OP_DRAM_INIT, OP_DRAM_WAKE};
use fstart_arch::x86::udelay;
use fstart_core::services::ServiceError;
use fstart_driver_spd::ddr3::Ddr3Attributes;
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    /// Fuse register 0x0c: the memory-rate limit the vendor MRC honors.
    FUSE_0C [ MEMORY_RATE OFFSET(23) NUMBITS(4) [] ]
];

/// The fused maximum data rate (vendor MRC `FUN_fffb4c40`: IOSF port 0x14
/// register 0x0c, bits 26:23). 1 = DDR3-800, 4 = DDR3-1333, else 1066.
pub fn fused_max_freq<P: ConfigRegionAccess>(iosf: &Iosf<P>) -> DdrFreq {
    let fuse = iosf.read(FUSE, 0x0c);
    let code = LocalRegisterCopy::<u32, FUSE_0C::Register>::new(fuse).read(FUSE_0C::MEMORY_RATE);
    fstart_log::debug!("baytrail: memory-rate fuse {:#010x} (code {})", fuse, code);
    match code {
        1 => DdrFreq::Ddr800,
        4 => DdrFreq::Ddr1333,
        _ => DdrFreq::Ddr1066,
    }
}

/// The fastest rate the DIMM and the fuse allow.
pub fn select_frequency(dimm: &Ddr3Attributes, fused: DdrFreq) -> DdrFreq {
    let rated = [DdrFreq::Ddr1333, DdrFreq::Ddr1066]
        .into_iter()
        .find(|freq| dimm.t_ck <= freq.tck())
        .unwrap_or(DdrFreq::Ddr800);
    rated.min(fused)
}

/// The lowest supported CAS latency covering tAA, from CL5.
pub fn select_cas_latency(dimm: &Ddr3Attributes, tck: u32) -> Result<u32, ServiceError> {
    let cl_min = dimm.t_aa.div_ceil(tck).max(5);
    (cl_min..=18)
        .find(|cl| (4..=18).contains(cl) && dimm.cas_supported & (1 << (cl - 4)) != 0)
        .ok_or_else(|| {
            fstart_log::error!("baytrail: no valid CAS latency for tAA {}", dimm.t_aa);
            ServiceError::HardwareError
        })
}

/// Clock cycles covering `time` (1/256 ns) at `tck`.
fn clocks(time: u32, tck: u32) -> u32 {
    time.div_ceil(tck)
}

pub fn program_timings<P: ConfigRegionAccess>(iosf: &Iosf<P>, p: &Params) {
    let d = &p.dimm;
    let tck = p.freq.tck();
    let (cl, wl) = (p.cas_latency, p.write_latency);
    let trp = clocks(d.t_rp, tck);
    let trcd = clocks(d.t_rcd, tck);
    let tras = clocks(d.t_ras, tck);
    let twr = clocks(d.t_wr, tck);
    let twtr = clocks(d.t_wtr, tck);
    let trrd = clocks(d.t_rrd, tck);
    let trtp = clocks(d.t_rtp, tck);
    let tfaw = clocks(d.t_faw, tck);

    // Scrambler off during init.
    iosf.reg(SCRMSEED_REG).write(SCRMSEED::DISABLE::SET);
    iosf.reg(SCRMLO).set(0);
    iosf.reg(SCRMHI).set(0);

    iosf.reg(DTR0_REG).modify(
        DTR0::DRAM_FREQ.val(p.freq as u32)
            + DTR0::T_RP.val(trp - 5)
            + DTR0::T_RCD.val(trcd - 5)
            + DTR0::T_CL.val(cl - 5)
            + DTR0::PMEDLY.val(3)
            + DTR0::CKEDLY.val(1),
    );

    let rtp_min = if p.freq <= DdrFreq::Ddr1066 { 4 } else { 5 };
    iosf.reg(DTR1_REG).modify(
        DTR1::T_WCL.val(wl - 3)
            // 1N command rate.
            + DTR1::T_CMD.val(0)
            + DTR1::T_WTP.val(wl + 4 + twr - 14)
            + DTR1::T_CCD.val(0)
            + DTR1::T_FAW.val(tfaw.div_ceil(2) - 5)
            + DTR1::T_RAS.val(tras - 14)
            + DTR1::T_RRD.val(trrd - 4)
            + DTR1::T_RTP.val(trtp.max(rtp_min) - 3),
    );

    iosf.reg(DTR2_REG).modify(
        DTR2::T_RRDR.val(1)
            + DTR2::T_RRDD.val(1)
            + DTR2::T_WWDR.val(2)
            + DTR2::T_WWDD.val(2)
            + DTR2::T_RWDR.val(2)
            + DTR2::T_RWDD.val(1),
    );

    // 1N: tCMD = 0.
    let (rwsr, wrsr) = match p.freq {
        DdrFreq::Ddr1333 => (3, 5),
        DdrFreq::Ddr1066 => (cl - 5, 3),
        DdrFreq::Ddr800 => (cl - 5, 4 + wl + twtr - 11 + 1),
    };
    let txp = if p.freq == DdrFreq::Ddr800 { 1 } else { 2 };
    iosf.reg(DTR3_REG).modify(
        DTR3::T_WRDR.val(2)
            + DTR3::T_WRDD.val(2)
            + DTR3::PWD_DLY.val(6)
            + DTR3::T_RWSR.val(rwsr)
            + DTR3::T_WRSR.val(wrsr)
            + DTR3::T_XP.val(txp),
    );

    let rdodt = (cl - 5) + 2 - (wl - 3);
    iosf.reg(DTR4_REG).modify(
        DTR4::WR_ODT_STRT.val(0)
            + DTR4::WR_ODT_STOP.val(0)
            + DTR4::RD_ODT_STRT.val(rdodt)
            + DTR4::RD_ODT_STOP.val(rdodt)
            + DTR4::TRGSTR_DIS::CLEAR
            + DTR4::RD_ODT_DIS::CLEAR
            + DTR4::WRB_ODT_DIS::CLEAR,
    );
}

/// Single channel arbitration, refresh and ZQ off, every rank enabled for
/// training and a temporary memory map below 3.5 GiB.
pub fn pre_jedec<P: ConfigRegionAccess>(iosf: &Iosf<P>, p: &Params) {
    iosf.reg(bunit::BMISC_REG).modify(BMISC::DUAL_CHAN.val(0));
    iosf.reg(bunit::BISOC_REG).modify(BISOC::PRIORITY.val(0x10));
    iosf.reg(DPMC0_REG).modify(DPMC0::BLMODE::SET);
    iosf.reg(PMSTS_REG).modify(PMSTS::SELF_REFRESH::CLEAR);
    iosf.reg(DRFC_REG).modify(DRFC::TREFI.val(0));
    iosf.reg(DCAL_REG)
        .modify(DCAL::ZQCINT.val(0) + DCAL::SRXZQCL.val(0));
    iosf.reg(DRP_REG).write(
        DRP::RANK_EN.val(0xf)
            + DRP::RANK_REMAP::SET
            + DRP::DIMM0_MIRROR.val(u32::from(p.dimm.mirrored)),
    );

    // 0xe00 MiB, TSEG at the MiB below it.
    const BMBOUND_MB: u32 = 0xe00;
    let tseg_mb = BMBOUND_MB - 1;
    iosf.reg(bunit::BMBOUND_REG)
        .modify(BMBOUND::ADDRESS.val(BMBOUND_MB >> 7));
    iosf.reg(bunit::BMBOUND_HI_REG)
        .modify(BMBOUND_HI::ADDRESS.val(1));
    iosf.reg(bunit::SMRRL).set(tseg_mb);
    let smrrh = iosf.reg(bunit::SMRRH);
    smrrh.set(smrrh.get() | tseg_mb | 1 << 31);
}

fn jedec_command<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    command: tock_registers::fields::FieldValue<u32, JEDEC::Register>,
) {
    iosf.message(DUNIT_CH0, OP_DRAM_INIT, command.value);
}

/// An address-mirrored DIMM swaps A3/A4, A5/A6, A7/A8 and BA0/BA1 on its
/// odd rank, so mode register writes to that rank carry the same swap. In
/// the DRAM init command word BA0 is bit 3 and MA0 bit 6.
fn jedec_mirror(command: u32) -> u32 {
    [(3, 4), (9, 10), (11, 12), (13, 14)]
        .into_iter()
        .fold(command, |command, (lo, hi)| {
            if (command >> lo) & 1 != (command >> hi) & 1 {
                command ^ (1 << lo | 1 << hi)
            } else {
                command
            }
        })
}

/// A mode register write to `rank`.
fn jedec_mrs<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    p: &Params,
    rank: u32,
    command: tock_registers::fields::FieldValue<u32, JEDEC::Register>,
) {
    let mut word = (command + JEDEC::RANK.val(rank)).value;
    if p.dimm.mirrored && rank & 1 != 0 {
        word = jedec_mirror(word);
    }
    iosf.message(DUNIT_CH0, OP_DRAM_INIT, word);
}

/// Reset, wake and mode-register programming of every populated rank.
pub fn jedec_init<P: ConfigRegionAccess>(iosf: &Iosf<P>, p: &Params) {
    fstart_log::info!("baytrail: JEDEC init");
    let twr = clocks(p.dimm.t_wr, p.freq.tck());
    // The reference updates one DRMC copy through the whole sequence.
    let drmc_reg = iosf.reg(DRMC_REG);
    let mut drmc = drmc_reg.extract();

    // DDR3 reset: cold wake, then release.
    drmc.modify(DRMC::COLD_WAKE::SET);
    drmc_reg.set(drmc.get());
    iosf.message(DUNIT_CH0, OP_DRAM_WAKE, 0);
    udelay(1000);
    drmc.modify(DRMC::COLD_WAKE::CLEAR);
    drmc_reg.set(drmc.get());
    udelay(1000);

    // CKE on for the enabled ranks.
    let ranks = iosf.reg(DRP_REG).read(DRP::RANK_EN);
    drmc.modify(DRMC::CKE_VAL.val(ranks) + DRMC::CKE_MODE::SET);
    drmc_reg.set(drmc.get());

    for rank in 0..p.ranks {
        jedec_command(iosf, JEDEC::COMMAND::Nop + JEDEC::RANK.val(rank));
    }
    udelay(1000);

    // Release the CKE/ODT overrides, keep bits 31:16 and 7:5, ODT mode on.
    drmc.modify(
        DRMC::CKE_VAL.val(0) + DRMC::CKE_MODE::CLEAR + DRMC::ODT_VAL.val(0) + DRMC::ODT_MODE::CLEAR,
    );
    drmc_reg.set((drmc.get() & 0xffff_00e0) | DRMC::ODT_MODE::SET.value);

    let cl_field = iosf.reg(DTR0_REG).read(DTR0::T_CL);
    for rank in 0..p.ranks {
        // MR2: CWL, dynamic ODT 120 ohm.
        jedec_mrs(
            iosf,
            p,
            rank,
            JEDEC::COMMAND::Mrs
                + JEDEC::BANK.val(2)
                + JEDEC::ADDRESS
                    .val((MR2::CWL.val(p.write_latency - 5) + MR2::RTT_WR.val(2)).value >> 6),
        );
        // MR3.
        jedec_mrs(iosf, p, rank, JEDEC::COMMAND::Mrs + JEDEC::BANK.val(3));
        // MR1: DLL on, 34 ohm, RTT_NOM 120 ohm (one rank) or 40 ohm.
        let rtt = if p.ranks == 1 {
            MR1::RTT_NOM1::SET
        } else {
            MR1::RTT_NOM0::SET + MR1::RTT_NOM1::SET
        };
        jedec_mrs(
            iosf,
            p,
            rank,
            JEDEC::COMMAND::Mrs
                + JEDEC::BANK.val(1)
                + JEDEC::ADDRESS.val((MR1::DIC0::SET + rtt).value >> 6),
        );
        // MR0: on-the-fly BL, CL, DLL reset, write recovery, fast exit.
        let wr = if twr < 10 { twr - 4 } else { (twr >> 1) & 7 };
        let ppd = if p.freq == DdrFreq::Ddr800 {
            MR0::PPD::CLEAR
        } else {
            MR0::PPD::SET
        };
        jedec_mrs(
            iosf,
            p,
            rank,
            JEDEC::COMMAND::Mrs
                + JEDEC::BANK.val(0)
                + JEDEC::ADDRESS.val(
                    (MR0::BL.val(1)
                        + MR0::CL_HIGH.val((cl_field + 1) & 7)
                        + MR0::DLL_RESET::SET
                        + MR0::WR.val(wr)
                        + ppd)
                        .value
                        >> 6,
                ),
        );
        // ZQ calibration long (MA10).
        jedec_command(
            iosf,
            JEDEC::COMMAND::Zqcal + JEDEC::ADDRESS.val(1 << 10) + JEDEC::RANK.val(rank),
        );
        udelay(1000);
    }

    iosf.reg(DCO_REG).modify(DCO::PMICTL::CLEAR + DCO::IC::SET);
}

/// BUNIT address-map tables for a 64-bit bus, by rank size 512 MiB to
/// 8 GiB: bank, row and rank masks and the rank pick mask.
const ADDR_MAP_64: [[u32; 4]; 5] = [
    [0x0e, 0x1fff0, 0x08, 0x0c],
    [0x0e, 0x3fff0, 0x10, 0x18],
    [0x0e, 0x7fff0, 0x20, 0x30],
    [0x0e, 0xffff0, 0x40, 0x60],
    [0x0e, 0xffff0, 0x40, 0xc0],
];

/// DRP/BC0AHASHCFG device width: x8 = 0, x16 = 1, x32 = 2.
pub fn width_encoding(width: u8) -> Result<u32, ServiceError> {
    match width {
        8 => Ok(0),
        16 => Ok(1),
        32 => Ok(2),
        _ => {
            fstart_log::error!("baytrail: unsupported SDRAM width x{}", width);
            Err(ServiceError::NotSupported)
        }
    }
}

/// DRP device density: 1 Gb = 0 .. 8 Gb = 3.
pub fn density_encoding(dimm: &Ddr3Attributes) -> Result<u32, ServiceError> {
    let devices_per_rank = 64 / u32::from(dimm.width);
    let mbit = dimm.size_mb / u32::from(dimm.ranks) / devices_per_rank * 8;
    match mbit {
        1024 => Ok(0),
        2048 => Ok(1),
        4096 => Ok(2),
        8192 => Ok(3),
        _ => {
            fstart_log::error!("baytrail: unsupported SDRAM density {} Mbit", mbit);
            Err(ServiceError::NotSupported)
        }
    }
}

fn addr_map_index(size_mb: u32) -> usize {
    [512, 1024, 2048, 4096]
        .iter()
        .position(|&limit| size_mb <= limit)
        .unwrap_or(4)
}

/// Graphics stolen memory, from the GGC raminit's pre-init programmed.
#[derive(Debug, Clone, Copy)]
pub struct Stolen {
    pub gsm_mb: u32,
    pub gtt_mb: u32,
}

/// The DRAM map raminit programmed (MiB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryMap {
    pub tom_mb: u32,
    pub bmbound_mb: u32,
    pub txe_base_mb: u32,
    pub txe_mb: u32,
    pub gsm_base_mb: u32,
    pub gtt_base_mb: u32,
    pub tseg_base_mb: u32,
    pub tseg_mb: u32,
}

/// Geometry, address map, stolen and TSEG carve-outs, refresh, power
/// management, and the locks.
pub fn finalize<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    p: &Params,
    stolen: Stolen,
    txe_mb: u32,
    tseg_mb: u32,
) -> Result<MemoryMap, ServiceError> {
    fstart_log::info!("baytrail: memory controller finalization");
    let d = &p.dimm;
    let width = width_encoding(d.width)?;
    let density = density_encoding(d)?;
    let dco = iosf.reg(DCO_REG);

    // DRP may only change while IC is clear.
    dco.modify(DCO::IC::CLEAR);
    let two_ranks = d.ranks > 1;
    iosf.reg(DRP_REG).write(
        DRP::RANK0_EN::SET
            + DRP::RANK1_EN.val(u32::from(two_ranks))
            + DRP::DIMM0_WIDTH.val(width)
            + DRP::DIMM0_DENSITY.val(density)
            + DRP::DIMM0_MIRROR.val(u32::from(d.mirrored))
            + DRP::RSIEN::SET
            + DRP::RANK_REMAP.val(u32::from(two_ranks)),
    );

    let rank_size_mb = d.size_mb / u32::from(d.ranks);
    let rank_size_128 = rank_size_mb / 128;
    let size_log = if rank_size_128 > 1 {
        rank_size_128.ilog2()
    } else {
        0
    };
    iosf.reg(bunit::BC0AHASHCFG_REG).write(
        BC0AHASHCFG::ADDR_HASH_EN::SET
            + BC0AHASHCFG::RANK_INTLV_EN::SET
            + BC0AHASHCFG::MEM_INT_WIDTH::SET
            + BC0AHASHCFG::RANK0_EN::SET
            + BC0AHASHCFG::RANK1_EN.val(u32::from(two_ranks))
            + BC0AHASHCFG::DIMM0_WIDTH.val(width)
            + BC0AHASHCFG::DIMM0_DENSITY.val(size_log),
    );

    // The masks split a system address by the geometry of one rank.
    let [bank, row, rank, pick] = ADDR_MAP_64[addr_map_index(rank_size_mb)];
    iosf.reg(bunit::BBANKMASK).set(bank);
    iosf.reg(bunit::BROWMASK).set(row);
    iosf.reg(bunit::BRANKMASK).set(rank);
    let bthctrl = iosf.reg(bunit::BTHCTRL_REG);
    bthctrl.modify(BTHCTRL::RANK_PICK_MASK.val(bthctrl.read(BTHCTRL::RANK_PICK_MASK) | pick));
    fstart_log::debug!(
        "baytrail: DRP={:#010x} HASH={:#010x} BANK={:#x} ROW={:#x} RANK={:#x}",
        iosf.reg(DRP_REG).get(),
        iosf.reg(bunit::BC0AHASHCFG_REG).get(),
        bank,
        row,
        rank
    );
    dco.modify(DCO::IC::SET);

    // Memory map: a 2 GiB MMIO hole below 4 GiB, the rest reclaimed.
    let tom_mb = d.size_mb;
    const MMIO_MB: u32 = 0x800;
    let bmbound_mb = (0x1000 - MMIO_MB).min(tom_mb);
    let bmbound_hi = if tom_mb > bmbound_mb {
        ((tom_mb + MMIO_MB) & !0xff) >> 8
    } else {
        tom_mb >> 8
    };
    iosf.reg(bunit::BMBOUND_REG)
        .modify(BMBOUND::ADDRESS.val(bmbound_mb >> 7));
    iosf.reg(bunit::BMBOUND_HI_REG)
        .modify(BMBOUND_HI::ADDRESS.val(bmbound_hi));

    // Top down below BMBOUND: TXE UMA, graphics stolen, GTT, TSEG.
    let txe_base_mb = bmbound_mb - txe_mb;
    let gsm_base_mb = txe_base_mb - stolen.gsm_mb;
    let gtt_base_mb = gsm_base_mb - stolen.gtt_mb;
    if txe_mb != 0 {
        // Fence the stolen regions off from the host cores.
        iosf.reg(bunit::bimr_low(0)).set(gtt_base_mb << 10);
        iosf.reg(bunit::bimr_high(0))
            .set(((txe_base_mb << 10) - 1) | 1 << 31);
        iosf.reg(bunit::bimr_rac(0)).set(bunit::IMR_AC_GFX);
        iosf.reg(bunit::bimr_wac(0)).set(bunit::IMR_AC_GFX);
        iosf.reg(bunit::bimr_low(1)).set(txe_base_mb << 10);
        iosf.reg(bunit::bimr_high(1))
            .set(((bmbound_mb << 10) - 1) | 1 << 31);
        iosf.reg(bunit::bimr_rac(1)).set(bunit::IMR_AC_SEC);
        iosf.reg(bunit::bimr_wac(1)).set(bunit::IMR_AC_SEC);
    }
    let tseg_base_mb = (gtt_base_mb - tseg_mb) & !(tseg_mb - 1);
    iosf.reg(bunit::SMRRL).set(tseg_base_mb);
    iosf.reg(bunit::SMRRH)
        .write(SMRR::MB.val(tseg_base_mb + tseg_mb - 1) + SMRR::ENABLE.val(3));
    // SMRAM: host cores only.
    iosf.reg(bunit::SMRRAC).set(0x14);
    iosf.reg(bunit::SMRWAC).set(0x14);
    let map = MemoryMap {
        tom_mb,
        bmbound_mb,
        txe_base_mb,
        txe_mb,
        gsm_base_mb,
        gtt_base_mb,
        tseg_base_mb,
        tseg_mb,
    };
    fstart_log::info!(
        "baytrail: TOM {}M BMBOUND {}M TXE {}M@{} GSM {}M@{} GTT {}M@{} TSEG {}M@{}",
        tom_mb,
        bmbound_mb,
        txe_mb,
        txe_base_mb,
        stolen.gsm_mb,
        gsm_base_mb,
        stolen.gtt_mb,
        gtt_base_mb,
        tseg_mb,
        tseg_base_mb
    );

    // Normal operation: no CKE/ODT forcing, refresh and periodic ZQ on.
    iosf.reg(DRMC_REG).modify(
        DRMC::COLD_WAKE::CLEAR
            + DRMC::CKE_VAL.val(0)
            + DRMC::CKE_MODE::CLEAR
            + DRMC::ODT_VAL.val(0)
            + DRMC::ODT_MODE::CLEAR,
    );
    iosf.reg(DRFC_REG).modify(
        DRFC::TREFI.val(3)
            + DRFC::REFWMLO.val(0xd)
            + DRFC::REFWMHI.val(0xd)
            + DRFC::REFWMPNC.val(0xd)
            + DRFC::REFCNTMAX.val(2)
            + DRFC::REFSKEWDIS::CLEAR
            + DRFC::REFDBTCLR::SET,
    );
    iosf.reg(DCAL_REG).modify(DCAL::ZQCINT.val(3));
    iosf.reg(DPMC0_REG).modify(
        DPMC0::SR_ENTRY_DLY.val(0x40)
            + DPMC0::PM_OPCODE.val(7)
            + DPMC0::PCLSTO.val(4)
            + DPMC0::PREAPWDEN::SET
            + DPMC0::DYNSREN::SET
            + DPMC0::PWRGATEDIS::SET
            + DPMC0::ENPHYCLKGATE::SET
            + DPMC0::ENCORECLKGATE::SET,
    );
    iosf.reg(DSCH_REG).modify(
        DSCH::OOODIS::CLEAR
            + DSCH::OOOST3DIS::SET
            + DSCH::OOOAGETRH.val(8)
            + DSCH::NEWBYPDIS::CLEAR
            + DSCH::IPREQMAX.val(7),
    );
    iosf.reg(DPMC1_REG)
        .modify(DPMC1::CMD_TRIST.val(2) + DPMC1::CS_TRIST::SET);
    iosf.reg(BONUS0_REG).modify(BONUS0::SLOWPDXEN::CLEAR);
    // E and F segments to DRAM; B0+ PnP setting.
    iosf.reg(bunit::BMISC_REG)
        .modify(BMISC::EDSEG_TO_DRAM::SET + BMISC::FDSEG_TO_DRAM::SET + BMISC::PNP.val(0xe));
    Ok(map)
}

/// Lock DRP and the REUT/CPGC configuration.
pub fn lock<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    iosf.reg(DCO_REG)
        .modify(DCO::DRPLOCK::SET + DCO::REUTLOCK::SET);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kingston() -> Ddr3Attributes {
        Ddr3Attributes {
            width: 16,
            ranks: 1,
            size_mb: 2048,
            t_ck: 320,
            t_aa: 0x69 * 32,
            cas_supported: 0xfe,
            ..Default::default()
        }
    }

    #[test]
    fn rate_follows_the_fuse_not_only_the_spd() {
        let dimm = kingston();
        assert_eq!(select_frequency(&dimm, DdrFreq::Ddr1333), DdrFreq::Ddr1333);
        assert_eq!(select_frequency(&dimm, DdrFreq::Ddr800), DdrFreq::Ddr800);
        assert_eq!(select_cas_latency(&dimm, DdrFreq::Ddr1333.tck()), Ok(9));
    }

    #[test]
    fn mirrored_rank_swaps_bank_and_address_pairs() {
        // BA0 (bit 3) set, A3 (bit 9) set, A6 (bit 12) set.
        let command = 1 << 3 | 1 << 9 | 1 << 12;
        assert_eq!(jedec_mirror(command), 1 << 4 | 1 << 10 | 1 << 11);
        // Equal pairs stay put.
        assert_eq!(jedec_mirror(0b11 << 3), 0b11 << 3);
    }

    #[test]
    fn geometry_uses_the_mrc_encodings() {
        // Four 4 Gb x16 devices, one rank: the board the vendor MRC decodes
        // as DRP width 1 and density 2.
        let dimm = kingston();
        assert_eq!(width_encoding(dimm.width), Ok(1));
        assert_eq!(density_encoding(&dimm), Ok(2));
        assert_eq!(addr_map_index(dimm.size_mb), 2);
    }
}
