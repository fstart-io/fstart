//! JEDEC DDR2 initialization sequences: MRS/EMRS commands, misc, ZQCL.
//!
//! Ported from coreboot `sdram_jedecinit()`, `sdram_misc()`,
//! `sdram_zqcl()`.

use super::SysInfo;
use fstart_pineview_regs::{mchbar, MchBar};

// DDR2 JEDEC command encodings (for C0JEDEC register).
const NOP_CMD: u8 = 1 << 1;
const PRE_CHARGE_CMD: u8 = 1 << 2;
const MRS_CMD: u8 = (1 << 2) | (1 << 1);
const EMRS1_CMD: u8 = 1 << 3;
const EMRS2_CMD: u8 = (1 << 3) | (1 << 2);
const EMRS3_CMD: u8 = (1 << 3) | (1 << 2) | (1 << 1);
const CBR_CMD: u8 = (1 << 3) | (1 << 2);
const NORMAL_OP_CMD: u8 = (1 << 3) | (1 << 2) | (1 << 1);

/// Send a DDR2 command via the JEDEC register.
///
/// Ported from coreboot `sdram_jedec()`. The command value is written
/// to bits [5:1] of C0JEDEC. The MRS/EMRS value is used as a
/// 32-bit address that is read (triggering the DRAM command).
fn send_jedec_cmd(mch: &MchBar, rank: u8, jmode: u8, jval: u16) {
    let addr: u32 = (jval as u32) << 3 | (rank as u32) * (1 << 27);

    let v = mch.read8(mchbar::C0JEDEC);
    mch.write8(mchbar::C0JEDEC, (v & !0x3E) | jmode);

    // Issue the command by reading from the computed address.
    // On real hardware this triggers the DRAM command via the MC.
    // SAFETY: This is a memory-mapped DRAM strobe — the address is
    // computed from the JEDEC spec and rank geometry.
    unsafe {
        core::ptr::read_volatile(addr as *const u32);
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);

    // 1 µs delay for command execution.
    for _ in 0..200 {
        core::hint::spin_loop();
    }
}

/// JEDEC DDR2 initialization sequence.
///
/// Ported from coreboot `sdram_jedecinit()`.
///
/// The standard DDR2 init sequence is:
/// 1. NOP (stabilize clocks)
/// 2. Precharge all
/// 3. EMRS2 (extended mode register set 2)
/// 4. EMRS3 (extended mode register set 3)
/// 5. EMRS1 (enable DLL, ODT settings)
/// 6. MRS (set CAS, burst length, write recovery, DLL reset)
/// 7. Precharge all
/// 8. Two auto-refresh cycles
/// 9. MRS (clear DLL reset)
/// 10. EMRS1 (OCD calibration default, then exit)
pub fn jedec_init(si: &SysInfo, mch: &MchBar) {
    let cas = si.selected_timings.cas;
    let twr = si.selected_timings.twr;

    // MRS value: CAS[6:4] | WR[11:9] | DLL reset[8] | BL=4 interleaved[1:0]=3 | BT[3]=1
    let mrs: u16 = ((cas as u16) << 4)
        | (((twr.wrapping_sub(1)) as u16) << 9)
        | (1 << 8) // DLL reset
        | (1 << 3) // BT = interleaved
        | 0x03; // BL = 4 (interleaved) + trailing 1

    // RTT_NOM: 150 Ω (bit 2). Dual-DIMM adds bit 6 for stronger
    // termination (50 Ω) due to additional bus loading.  This
    // matches coreboot’s `rank_is_populated(dimms, 0, 0) &&
    // rank_is_populated(dimms, 0, 2)` check — rank 2 = DIMM 1’s
    // first rank, so the condition is "both DIMMs populated".
    // A single dual-rank DIMM keeps 150 Ω (correct for DDR2 ODT).
    let mut rttnom: u16 = 1 << 2;
    let d0_pop = si.dimm_populated(0);
    let d1_pop = si.dimm_populated(1);
    if d0_pop && d1_pop {
        rttnom |= 1 << 6;
    }

    // 200 µs settling time.
    for _ in 0..40_000 {
        core::hint::spin_loop();
    }

    // Execute JEDEC sequence for each populated rank.
    for r in 0..super::RANKS_PER_CHANNEL {
        let dimm_idx = r / 2;
        let rank_in_dimm = (r % 2) as u8;
        let populated = si.dimms[dimm_idx]
            .as_ref()
            .map_or(false, |d| d.card_type != 0 && rank_in_dimm < d.ranks);
        if !populated {
            continue;
        }

        let rank = r as u8;

        // 1. NOP
        send_jedec_cmd(mch, rank, NOP_CMD, 0);
        // 2. Precharge all
        send_jedec_cmd(mch, rank, PRE_CHARGE_CMD, 0);
        // 3. EMRS2
        send_jedec_cmd(mch, rank, EMRS2_CMD, 0);
        // 4. EMRS3
        send_jedec_cmd(mch, rank, EMRS3_CMD, 0);
        // 5. EMRS1 — DLL enable, RTT_NOM
        send_jedec_cmd(mch, rank, EMRS1_CMD, rttnom);
        // 6. MRS — CAS, BL, WR, DLL reset
        send_jedec_cmd(mch, rank, MRS_CMD, mrs);
        // 7. Precharge all
        send_jedec_cmd(mch, rank, PRE_CHARGE_CMD, 0);
        // 8. Two auto-refresh
        send_jedec_cmd(mch, rank, CBR_CMD, 0);
        send_jedec_cmd(mch, rank, CBR_CMD, 0);
        // 9. MRS — clear DLL reset (remove bit 8)
        send_jedec_cmd(mch, rank, MRS_CMD, mrs & !(1 << 8));
        // 10. EMRS1 — OCD calibration default (bits 9:7 = 111), then exit
        send_jedec_cmd(mch, rank, EMRS1_CMD, rttnom | (7 << 7));
        send_jedec_cmd(mch, rank, EMRS1_CMD, rttnom);
    }

    fstart_log::info!("raminit: JEDEC init complete (CAS={}, WR={})", cas, twr);
}

/// Miscellaneous post-JEDEC setup.
///
/// Ported from coreboot `sdram_misc()`.
pub fn sdram_misc(si: &SysInfo, mch: &MchBar) {
    let reg32 = (4u32 << 13) | (6 << 8);
    let v = mch.read32(mchbar::C0DYNRDCTRL);
    mch.write32(mchbar::C0DYNRDCTRL, (v & !(0x3FF << 8)) | reg32);
    let v = mch.read8(mchbar::C0DYNRDCTRL);
    mch.write8(mchbar::C0DYNRDCTRL, v & !(1 << 7));
    // Byte access — coreboot: mchbar_setbits8(C0REFRCTRL + 3, 1 << 0)
    let v = mch.read8(mchbar::C0REFRCTRL + 3);
    mch.write8(mchbar::C0REFRCTRL + 3, v | (1 << 0));

    if si.boot_path != super::BOOT_PATH_RESUME {
        // Normal/Reset path: set NORMAL_OP command.
        let v = mch.read8(mchbar::C0JEDEC);
        mch.write8(mchbar::C0JEDEC, (v & !0x0E) | NORMAL_OP_CMD);
        let v = mch.read8(mchbar::C0JEDEC);
        mch.write8(mchbar::C0JEDEC, v & !(3 << 4));
    } else {
        // S3 Resume path: run ZQCL instead of JEDEC normal-op.
        sdram_zqcl(si, mch);
    }

    fstart_log::info!("raminit: misc configured");
}

/// ZQCL (ZQ calibration long) command — S3 resume path.
///
/// Ported from coreboot `sdram_zqcl()`.
pub fn sdram_zqcl(si: &SysInfo, mch: &MchBar) {
    if si.boot_path == super::BOOT_PATH_RESUME {
        mch.setbits32(mchbar::C0CKECTRL, 1 << 27);
        let v = mch.read8(mchbar::C0JEDEC);
        mch.write8(mchbar::C0JEDEC, (v & !0x0E) | NORMAL_OP_CMD);
        let v = mch.read8(mchbar::C0JEDEC);
        mch.write8(mchbar::C0JEDEC, v & !(3 << 4));
        mch.setbits32(mchbar::C0REFRCTRL2, 3 << 30);
    }

    fstart_log::info!("raminit: ZQCL done");
}
