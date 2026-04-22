//! JEDEC DDR2 initialization sequences: MRS/EMRS commands, misc, ZQCL.
//!
//! Ported from coreboot `sdram_jedecinit()`, `sdram_misc()`,
//! `sdram_zqcl()`.

use super::SysInfo;
use fstart_pineview_regs::{mchbar, MchBar};

// DDR2 JEDEC command encodings (for C0JEDEC register).
const NOP_CMD: u32 = 1 << 1;
const PRE_CHARGE_CMD: u32 = 1 << 2;
const MRS_CMD: u32 = (1 << 2) | (1 << 1);
const EMRS_CMD: u32 = 1 << 3;
const EMRS1_CMD: u32 = EMRS_CMD | (1 << 4);
const EMRS2_CMD: u32 = EMRS_CMD | (1 << 5);
const EMRS3_CMD: u32 = EMRS_CMD | (1 << 5) | (1 << 4);
const ZQCAL_CMD: u32 = (1 << 3) | (1 << 1);
const CBR_CMD: u32 = (1 << 3) | (1 << 2);
const NORMAL_OP_CMD: u32 = (1 << 3) | (1 << 2) | (1 << 1);

/// Send a DDR2 command via the JEDEC register.
fn send_jedec_cmd(mch: &MchBar, cmd: u32, data: u32) {
    mch.write32(mchbar::C0JEDEC, cmd | data);
    // The controller needs time to execute the command.
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
/// 6. MRS (set CAS, burst length, DLL reset)
/// 7. Precharge all
/// 8. Two auto-refresh cycles
/// 9. MRS (clear DLL reset)
/// 10. EMRS1 (OCD calibration default, then exit)
/// 11. Normal operation
pub fn jedec_init(si: &SysInfo, mch: &MchBar) {
    let cas = si.selected_timings.cas as u32;
    let wr = si.selected_timings.twr as u32;

    // 1. NOP
    send_jedec_cmd(mch, NOP_CMD, 0);

    // 2. Precharge all
    send_jedec_cmd(mch, PRE_CHARGE_CMD, 1 << 10);

    // 3. EMRS2
    send_jedec_cmd(mch, EMRS2_CMD, 0);

    // 4. EMRS3
    send_jedec_cmd(mch, EMRS3_CMD, 0);

    // 5. EMRS1 — DLL enable, ODT = 150 ohm (bit 2 + bit 6)
    send_jedec_cmd(mch, EMRS1_CMD, (1 << 2) | (1 << 6));

    // 6. MRS — CAS, burst length = 4 (interleaved), DLL reset
    let mrs_val = (cas << 4) | (1 << 8) | (wr << 9) | 0x2; // BL=4
    send_jedec_cmd(mch, MRS_CMD, mrs_val);

    // 7. Precharge all
    send_jedec_cmd(mch, PRE_CHARGE_CMD, 1 << 10);

    // 8. Two auto-refresh
    send_jedec_cmd(mch, CBR_CMD, 0);
    send_jedec_cmd(mch, CBR_CMD, 0);

    // 9. MRS — clear DLL reset (remove bit 8)
    let mrs_val = (cas << 4) | (wr << 9) | 0x2;
    send_jedec_cmd(mch, MRS_CMD, mrs_val);

    // 10. EMRS1 — OCD calibration default (bits 9:7 = 111), then exit (000)
    send_jedec_cmd(mch, EMRS1_CMD, (1 << 2) | (1 << 6) | (7 << 7));
    send_jedec_cmd(mch, EMRS1_CMD, (1 << 2) | (1 << 6));

    // 11. Normal operation
    send_jedec_cmd(mch, NORMAL_OP_CMD, 0);

    fstart_log::info!("raminit: JEDEC init complete (CAS={}, WR={})", cas, wr);
}

/// Miscellaneous post-JEDEC setup.
///
/// Ported from coreboot `sdram_misc()`.
pub fn sdram_misc(si: &SysInfo, mch: &MchBar) {
    // Enable read/write FIFOs.
    mch.write32(mchbar::C0RDFIFOCTRL, 0x01);
    mch.write32(mchbar::C0WRDATACTRL, 0x01);

    // Address decoding miscellaneous.
    mch.write8(mchbar::CHDECMISC, 0x01);

    // Bonus registers.
    mch.write32(mchbar::C0COREBONUS, 0x0000_0400);

    fstart_log::info!("raminit: misc configured");
}

/// ZQCL (ZQ calibration long) command.
///
/// Ported from coreboot `sdram_zqcl()`.
pub fn sdram_zqcl(si: &SysInfo, mch: &MchBar) {
    send_jedec_cmd(mch, ZQCAL_CMD, 1 << 10);
    fstart_log::info!("raminit: ZQCL done");
}
