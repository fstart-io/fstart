//! MPLL selection, SPD timing conversion and MCMAIN timing/refresh registers.

use super::{Error, MchBar, mc, spd::Topology, wait_for_first_rcomp};
use crate::haswell::registers::*;
use fstart_driver_spd::ddr3::Ddr3ModuleType;

/// (tCK in 1/256 ns, nominal MHz, reference MHz), fastest first. These are
/// the JEDEC rounded periods, not reciprocals of the rounded MHz values.
const SPEEDS: [(u32, u32, u32); 11] = [
    (212, 1200, 133),
    (232, 1100, 100),
    (240, 1066, 133),
    (256, 1000, 100),
    (274, 933, 133),
    (284, 900, 100),
    (320, 800, 133),
    (365, 700, 100),
    (384, 666, 133),
    (480, 533, 133),
    (640, 400, 133),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub tck: u32,
    pub mhz: u32,
    pub reference: u32,
    pub multiplier: u32,
    pub cas: u32,
}
impl Clock {
    pub fn period_fs(self) -> u32 {
        1_000_000_000 / self.mhz
    }
    /// SMBIOS uses the actual reference (133 1/3 MHz), rather than the
    /// nominal table-bin label used by the training equations.
    pub fn transfer_rate_mts(self) -> u16 {
        let ref_khz = if self.reference == 100 {
            100000
        } else {
            133333
        };
        (2 * ((self.multiplier * ref_khz + 500) / 1000)) as u16
    }
    pub fn qclk_ps(self) -> u32 {
        self.period_fs() / 2000
    }
    pub fn candidates(t: &Topology, capid_b: u32) -> impl Iterator<Item = Self> + '_ {
        let min_tck = t.dimms().map(|d| d.tck_min_256ns).max().unwrap_or(0);
        let taa = t.dimms().map(|d| d.taa_min_256ns).max().unwrap_or(0);
        let trrd = t.dimms().map(|d| d.trrd_256ns).max().unwrap_or(0);
        SPEEDS.iter().filter_map(move |&(tck, mhz, reference)| {
            if tck < min_tck
                || trrd.div_ceil(tck) > TC_BANK::TRRD.mask
                || (reference == 100 && PCI_CAPID0_B::PLL_REF100_CFG.read(capid_b) == 0)
            {
                return None;
            }
            let cas = (taa.div_ceil(tck).max(4)..=19).find(|&cl| {
                let cwl = cwl(mhz, cl);
                t.cas & (1 << cl) != 0 && cwl <= 12 && (cwl..=cwl + 4).contains(&cl)
            })?;
            let mult = 256000 / (tck * reference);
            Some(Clock {
                tck,
                mhz,
                reference,
                multiplier: if reference == 100 {
                    mult.clamp(7, 12)
                } else {
                    mult.clamp(3, 10)
                },
                cas,
            })
        })
    }
}

/// On restore the saved clock must lock exactly; never silently use slower
/// hardware with trained PI codes from the faster clock.
pub fn initialise_mpll(
    mch: MchBar,
    t: &Topology,
    capid_b: u32,
    restored: Option<Clock>,
) -> Result<Clock, Error> {
    use MC_BIOS_REQ as req;
    let idle = || {
        super::poll(|| !req::RUN_BUSY.is_set(mch.read32(0x5e00)), 5001).map_err(|_| Error::PllBusy)
    };
    let clocks = || Clock::candidates(t, capid_b).filter(|c| restored.is_none_or(|old| *c == old));
    // The reference selector lives in REQ, not DATA. Once DATA reports a
    // multiplier the PLL cannot be reprogrammed, even on a cold retry.
    let locked = || {
        let mult = MC_BIOS_DATA::MULTIPLIER.read(mch.read32(0x5e04));
        if mult == 0 {
            return Ok(None);
        }
        let ref100 = req::REF100.is_set(mch.read32(0x5e00));
        let c = clocks()
            .find(|c| c.multiplier == mult && (c.reference == 100) == ref100)
            .ok_or(Error::Pll)?;
        wait_for_first_rcomp(mch)?;
        Ok(Some(c))
    };
    idle()?;
    if let Some(c) = locked()? {
        return Ok(c);
    }
    for clock in clocks() {
        mch.write32(
            0x5e00,
            (req::RUN_BUSY::SET
                + req::REF100.val(u32::from(clock.reference == 100))
                + req::MULTIPLIER.val(clock.multiplier))
            .value,
        );
        idle()?;
        if let Some(c) = locked()? {
            return Ok(c);
        }
        // Completed without a lock: the next (slower) request is safe.
    }
    Err(Error::Pll)
}

const BINS: [u32; 7] = [400, 533, 666, 800, 933, 1066, 1200];
fn lookup(mhz: u32, values: [u32; 8]) -> u32 {
    values[BINS.iter().position(|&f| mhz <= f + 5).unwrap_or(7)]
}

fn cwl(mhz: u32, cas: u32) -> u32 {
    lookup(mhz, [5, 6, 7, 8, 9, 10, 11, 12]).max(cas.saturating_sub(4))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    pub aa: u32,
    pub wr: u32,
    pub rcd: u32,
    pub rrd: u32,
    pub rp: u32,
    pub ras: u32,
    pub rc: u32,
    pub rfc: u32,
    pub wtr: u32,
    pub rtp: u32,
    pub faw: u32,
    pub cwl: u32,
    pub cmd: u32,
    pub refi: u32,
    pub xp: u32,
}
impl Timings {
    pub fn from_spd(t: &Topology, c: Clock, capid_a: u32) -> Self {
        let convert = |field: fn(&fstart_driver_spd::ddr3::Ddr3DimmInfo) -> u32, min, max| {
            t.dimms()
                .map(field)
                .max()
                .unwrap_or(0)
                .div_ceil(c.tck)
                .clamp(min, max)
        };
        let wr = convert(|d| d.twr_256ns, 5, 16);
        Self {
            aa: c.cas,
            wr: wr + u32::from(matches!(wr, 9 | 11 | 13 | 15)),
            rcd: convert(|d| d.trcd_256ns, 4, 20),
            // Candidate selection already rejected bins needing more than four bits.
            rrd: convert(|d| d.trrd_256ns, 4, TC_BANK::TRRD.mask),
            rp: convert(|d| d.trp_256ns, 4, 15),
            ras: convert(|d| d.tras_256ns, 10, 40),
            rc: convert(|d| d.trc_256ns, 1, 4095),
            rfc: convert(|d| d.trfc_256ns, 1, 511),
            wtr: convert(|d| d.twtr_256ns, 4, 10),
            rtp: convert(|d| d.trtp_256ns, 4, 15),
            faw: convert(|d| d.tfaw_256ns, 10, 54),
            cwl: cwl(c.mhz, c.cas),
            cmd: if c.mhz > 933 || t.dpc.contains(&2) || PCI_CAPID0_A::D1NM.is_set(capid_a) {
                2
            } else {
                1
            },
            refi: c.mhz * 7800 / 1000,
            xp: lookup(c.mhz, [3, 4, 4, 5, 6, 7, 8, 8]),
        }
    }

    /// Bank/rank timings during training (3N command stretch, raised tXP).
    pub fn configure(self, mch: MchBar, t: &Topology, c: Clock) {
        use TC_BANK as bank;
        use TC_BANK_RANK_A as a;
        use TC_BANK_RANK_B as b;
        use TC_BANK_RANK_C as rc;
        use TC_BANK_RANK_D as d;
        let stretch=u32::from(t.dimms.iter().any(|slots| matches!(slots,[Some(x),Some(y)] if [x,y].iter().all(|d|matches!(d.module_type,Ddr3ModuleType::Sodimm|Ddr3ModuleType::SoUdimm72b)) && x.raw_card!=y.raw_card && (x.raw_card==5||y.raw_card==5))));
        let rdwr = self.aa + 7 + u32::from(c.mhz > 666) - self.cwl;
        let wrrd = self.cwl + 7 - self.aa;
        let tc = (bank::TRCD.val(self.rcd)
            + bank::TRP.val(self.rp)
            + bank::TRAS.val(self.ras)
            + bank::TRDPRE.val(self.rtp)
            + bank::TWRPRE.val(4 + self.cwl + self.wr)
            + bank::TRRD.val(self.rrd))
        .value;
        let ta = (a::TCKE.val(lookup(c.mhz, [3, 3, 4, 4, 5, 6, 6, 7]))
            + a::TFAW.val(self.faw)
            + a::TRDRD_SR.val(4)
            + a::TRDRD_DR.val(6 + stretch)
            + a::TRDRD_DD.val(6 + stretch)
            + a::TRDPDEN.val(self.aa + 5)
            + a::CMD_3ST_DIS::SET
            + a::CMD_STRETCH.val(3))
        .value;
        let dec = u32::from(self.cwl >= 6);
        let tb = (b::TWRRD_SR.val(6 + self.cwl + self.wtr)
            + b::TWRRD_DR.val(wrrd)
            + b::TWRRD_DD.val(wrrd)
            + b::TWRWR_SR.val(4)
            + b::TWRWR_DR.val(7)
            + b::TWRWR_DD.val(7)
            + b::TWRPDEN.val(self.wr + self.cwl + 4)
            + b::DEC_WRD.val(dec))
        .value;
        let tc_c = (rc::TXPDLL.val(lookup(c.mhz, [10, 13, 16, 20, 23, 26, 29, 32]))
            + rc::TXP.val(self.xp.max(7))
            + rc::TAONPD.val(lookup(c.mhz, [4, 5, 6, 7, 8, 10, 11, 12]))
            + rc::TRDWR_SR.val(rdwr)
            + rc::TRDWR_DR.val(rdwr + stretch)
            + rc::TRDWR_DD.val(rdwr + stretch))
        .value;
        let td = (d::TAA.val(self.aa)
            + d::TCWL.val(self.cwl)
            + d::TCPDED.val(1)
            + d::TPRPDEN.val(1)
            + d::ODT_READ_DELAY.val(self.aa - self.cwl)
            + d::ODT_READ_DURATION.val(stretch))
        .value;
        for ch in t.channels() {
            for (off, val) in [
                (0x4000, tc),
                (0x4004, ta),
                (0x4008, tb),
                (0x400c, tc_c),
                (0x4014, td),
            ] {
                mch.write32(mc(off, ch), val);
            }
            mch.write8(mc(0x40d0, ch), (dec as u8 + 1) * 0x55);
        }
    }
    pub fn configure_refresh(self, mch: MchBar, t: &Topology, c: Clock, ult: bool) {
        let zq = lookup(c.mhz, [256, 256, 256, 256, 299, 342, 384, 427]);
        let sr = (TC_SRFTP::TXSDLL.val(512)
            + TC_SRFTP::TXS_OFFSET.val(c.mhz.div_ceil(100))
            + TC_SRFTP::TZQOPER.val(zq)
            + TC_SRFTP::TMOD.val(lookup(c.mhz, [12, 12, 12, 12, 14, 16, 18, 20]) - 8))
        .value;
        let rf = (TC_RFTP::TREFI.val(self.refi)
            + TC_RFTP::TRFC.val(self.rfc)
            + TC_RFTP::TREFIX9.val(self.refi * 89 / 10240))
        .value;
        for ch in t.channels() {
            mch.setbits32(mc(0x4294, ch), 0xff);
            mch.write32(mc(0x4298, ch), rf);
            mch.write32(mc(0x42a4, ch), sr);
            mch.write32(
                mc(0x4290, ch),
                (zq.div_ceil(4) << if ult { 10 } else { 8 }) | 128,
            );
        }
    }
}
