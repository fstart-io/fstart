//! Destructive cold-boot alias check and ECC parity initialization.
use super::{
    activate::normal,
    reut::write_qword,
    spd::Topology,
    testing_io::{Address, Axis, Stepping, rank_channels},
    *,
};
fn run(m: MchBar, mask: u8) -> Result<(), Error> {
    m.write32(
        0x4800,
        (REUT_SEQ_CTL::START_TEST::SET
            + REUT_SEQ_CTL::CLEAR_ERRORS::SET
            + REUT_SEQ_CTL::STOP_ON_ERROR::SET)
            .value,
    );
    let mut w = 0;
    poll(
        || {
            w = m.read32(0x4804);
            REUT_GLOBAL_ERR::CH_TEST_DONE.read(w) as u8 & mask == mask
        },
        10_000_000,
    )?;
    if REUT_GLOBAL_ERR::CH_ERROR.read(w) as u8 & mask != 0 {
        Err(Error::Scrub)
    } else {
        Ok(())
    }
}
/// Select all three LFSRs and the WDB ECC source. Cold alias-check sets
/// this before the scrub; a power-on cache hit has no preceding alias-check.
fn scrub_mux() -> u32 {
    use REUT_PAT_WDB_CL_MUX_CFG as mux;
    (mux::MUX_0_CONTROL.val(2)
        + mux::MUX_1_CONTROL.val(2)
        + mux::MUX_2_CONTROL.val(2)
        + mux::ECC_DATA_SOURCE_SEL::SET)
        .value
}
pub fn ecc_scrub(m: MchBar, t: &Topology, step: Stepping) -> Result<(), Error> {
    if t.lanes != 9 {
        return Ok(());
    }
    wdb::fixed_pattern(m, t, &[0], &[0], 0);
    use REUT_SEQ_CFG as cfg;
    for ch in t.channels() {
        write_qword(
            m,
            0x48a8 + 8 * ch as u32,
            (cfg::STOP_BASE_SEQ_ON_WRAP_TRIGGER::SET
                + cfg::ADDRESS_UPDATE_RATE_MODE::SET
                + cfg::GLOBAL_CONTROL::SET
                + cfg::INITIALIZATION_MODE.val(1)
                + cfg::START_TEST_DELAY.val(2))
            .value,
        );
        step.loop_count(m, ch, 255);
        m.write32(mc(0x4040, ch), scrub_mux());
        m.write32(
            0x4808 + 40 * ch as u32,
            REUT_SUBSEQ_CTL::SUBSEQ_TYPE.val(1).value,
        );
        m.write32(mc(0x4200, ch), REUT_PAT_WDB_CL_CTRL::INC_SCALE::SET.value);
    }
    let result = (|| {
        for r in 0..4 {
            let mask = rank_channels(m, t, r);
            if mask == 0 {
                continue;
            }
            for bank in 0..8 {
                for ch in t.channels().filter(|&ch| mask & (1 << ch) != 0) {
                    let d = t.dimms[ch][r / 2].as_ref().ok_or(Error::Scrub)?;
                    let cols = 1 << d.cols;
                    Address {
                        bank: Axis {
                            start: bank,
                            stop: bank,
                            wrap: true,
                            ..Axis::default()
                        },
                        row: Axis {
                            stop: (1 << d.rows) - 1,
                            inc: 1,
                            rate: cols / 8,
                            ..Axis::default()
                        },
                        col: Axis {
                            stop: cols - 8,
                            inc: 1,
                            ..Axis::default()
                        },
                        ..Address::default()
                    }
                    .program(m, ch);
                }
                run(m, mask)?;
            }
        }
        Ok(())
    })();
    for ch in 0..2 {
        normal(m, ch);
    }
    result
}
pub fn alias_check(m: MchBar, t: &Topology, step: Stepping) -> Result<(), Error> {
    let mut mad = [0; 2];
    if t.lanes == 9 {
        for ch in t.channels() {
            let reg = 0x5004 + 4 * ch as u32;
            mad[ch] = m.read32(reg);
            m.write32(
                reg,
                (MAD_DIMM::ECC_IO_ON::CLEAR + MAD_DIMM::ECC_LOGIC_ON::CLEAR).modify(mad[ch]),
            );
        }
    }
    wdb::setup(m, t);
    use REUT_SEQ_CFG as cfg;
    use REUT_SUBSEQ_CTL as ss;
    for ch in t.channels() {
        write_qword(
            m,
            0x48a8 + 8 * ch as u32,
            (cfg::GLOBAL_CONTROL::SET
                + cfg::INITIALIZATION_MODE.val(1)
                + cfg::SUBSEQUENCE_END_POINTER.val(1)
                + cfg::START_TEST_DELAY.val(2))
            .value,
        );
        step.loop_count(m, ch, 0);
        m.write32(mc(0x4200, ch), REUT_PAT_WDB_CL_CTRL::INC_SCALE::SET.value);
        m.write32(mc(0x4040, ch), scrub_mux());
    }
    let result = (|| {
        for slot in 0..2 {
            let mask = rank_channels(m, t, slot * 2);
            if mask == 0 {
                continue;
            }
            for ch in t.channels().filter(|&ch| mask & (1 << ch) != 0) {
                let d = t.dimms[ch][slot].as_ref().ok_or(Error::Scrub)?;
                let rows = 1u32 << d.rows;
                let cols = 1u32 << d.cols;
                let writes = cols / 256;
                Address {
                    bank: Axis {
                        start: 1,
                        stop: 1,
                        ..Axis::default()
                    },
                    row: Axis {
                        stop: rows - 1,
                        rate: writes - 1,
                        inc: 1024,
                        ..Axis::default()
                    },
                    col: Axis {
                        stop: cols - 256,
                        inc: 32,
                        ..Axis::default()
                    },
                    ..Address::default()
                }
                .program(m, ch);
                let num = rows / 1024 * writes - 1;
                let len = ss::NUM_CACHELINES_WITH_SCALE.val(u32::BITS - (num - 1).leading_zeros());
                m.write32(
                    0x4808 + 40 * ch as u32,
                    (len + ss::SUBSEQ_TYPE.val(1)).value,
                );
                m.write32(
                    0x480c + 40 * ch as u32,
                    (len + ss::RESET_CURR_BASE_ADDR_TO_START::SET).value,
                );
            }
            run(m, mask)?;
        }
        Ok(())
    })();
    if t.lanes == 9 {
        for ch in t.channels() {
            m.write32(0x5004 + 4 * ch as u32, mad[ch]);
        }
        fstart_arch::x86::udelay(4);
    }
    for ch in 0..2 {
        normal(m, ch);
    }
    result
}
