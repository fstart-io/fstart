//! LDAT writes and WDB victim/aggressor and LFSR patterns (`setup_wdb.c`).
use super::{LDAT_PDAT, LDAT_SDAT, MchBar, REUT_PAT_CL_MUX_LMN, mc, spd::Topology};

fn write_chunk(mch: MchBar, t: &Topology, chunk: u32, line: u32, data: u64) {
    for ch in t.channels() {
        // LDAT_DATA_IN is not 8-byte aligned. Commit the two dwords with
        // SDAT (bank=chunk, MC WDB array), then PDAT (write command 8).
        mch.write32(mc(0x42dc, ch), data as u32);
        mch.write32(mc(0x42e0, ch), (data >> 32) as u32);
        mch.write32(
            mc(0x42d4, ch),
            (LDAT_SDAT::BANK_SEL.val(chunk) + LDAT_SDAT::MODE.val(1)).value,
        );
        mch.write32(
            mc(0x42d0, ch),
            (LDAT_PDAT::FAST_ADDR.val(line.min(0xfff)) + LDAT_PDAT::CMD_B.val(8)).value,
        );
    }
}
fn clear(mch: MchBar) {
    for ch in 0..2 {
        mch.write32(mc(0x42d4, ch), 0);
    }
}

pub fn fixed_pattern(mch: MchBar, t: &Topology, patterns: &[u8], mask: &[u8], line: u32) {
    assert!(!mask.is_empty() && mask.iter().all(|&i| (i as usize) < patterns.len()));
    for chunk in 0..8 {
        let mut data = 0;
        for bit in 0..64 {
            if patterns[mask[bit % mask.len()] as usize] & (1 << chunk) != 0 {
                data |= 1u64 << bit;
            }
        }
        write_chunk(mch, t, chunk, line, data);
    }
    clear(mch);
}

pub fn va_pattern(
    mch: MchBar,
    t: &Topology,
    mut aggressor: u32,
    mut victim: u32,
    rotations: u8,
    start: u32,
) {
    for v in 0..u32::from(rotations) {
        for chunk in 0..8 {
            let mut data = 0u32;
            for bit in 0..32 {
                let i = (((!victim >> bit) & 1) << 1) | ((aggressor >> bit) & 1);
                data |= ((([0xaa, 0xc0, 0xcc, 0xf0][i as usize] >> chunk) & 1) as u32) << bit;
            }
            write_chunk(
                mch,
                t,
                chunk,
                start + v,
                (u64::from(data) << 32) | u64::from(data),
            );
        }
        aggressor = aggressor.rotate_left(1);
        victim = victim.rotate_left(1);
    }
    clear(mch);
}

pub fn program_lfsr(mch: MchBar, t: &Topology, cleanup: bool) {
    let seeds = if cleanup {
        [0xaaaaaa, 0xcccccc, 0xf0f0f0]
    } else {
        [0xa10ca1, 0xef0d08, 0xad0a1e]
    };
    for ch in t.channels() {
        for (i, s) in seeds.iter().enumerate() {
            mch.write32(mc(0x4054 + 4 * i as u32, ch), *s);
            mch.write32(mc(0x4048 + 4 * i as u32, ch), *s);
        }
    }
}
pub fn setup(mch: MchBar, t: &Topology) {
    va_pattern(mch, t, 0, 0x01010101, 8, 0);
    for (i, a) in [
        0x86186186, 0x18618618, 0x30c30c30, 0xa28a28a2, 0x8a28a28a, 0x14514514, 0x28a28a28,
        0x92492492, 0x24924924,
    ]
    .iter()
    .enumerate()
    {
        va_pattern(mch, t, *a, 0x41041041, 6, 8 + i as u32 * 6);
    }
    program_lfsr(mch, t, false);
    let cfg = (REUT_PAT_CL_MUX_LMN::EN_SWEEP_FREQ::SET
        + REUT_PAT_CL_MUX_LMN::L_COUNTER.val(1)
        + REUT_PAT_CL_MUX_LMN::M_COUNTER.val(1)
        + REUT_PAT_CL_MUX_LMN::N_COUNTER.val(10))
    .value;
    for ch in t.channels() {
        mch.write32(mc(0x4078, ch), cfg);
    }
}
