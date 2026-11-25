use super::{
    spd::{Slot, collect},
    timings::{Clock, Timings},
    *,
};

pub(super) fn host_mch(aperture: &mut [u64; crate::haswell::MCHBAR_SIZE / 8]) -> MchBar {
    // SAFETY: `aperture` is an 8-byte-aligned 32 KiB RAM buffer owned by the test.
    let win = unsafe {
        fstart_core::mmio::MmioWindow::new(
            aperture.as_mut_ptr() as usize,
            crate::haswell::MCHBAR_SIZE as u32,
        )
    };
    MchBar::from_window(win)
}

pub(super) fn fixture(text: &str) -> [u8; 256] {
    let mut out = [0; 256];
    let mut values = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .flat_map(str::split_ascii_whitespace);
    for v in &mut out {
        *v = u8::from_str_radix(values.next().unwrap(), 16).unwrap();
    }
    assert!(values.next().is_none());
    out
}
fn update_crc(d: &mut [u8; 256]) {
    let crc = spd::crc(d);
    d[126..128].copy_from_slice(&crc.to_le_bytes());
}
pub(super) fn peppy(slots: [[Slot<'_>; 2]; 2]) -> spd::Topology {
    collect(slots, None, 0, true, &mut [[[0; 256]; 2]; 2]).unwrap()
}
pub(super) const HYNIX: &str = include_str!("testdata/Hynix_HMT425S6AFR6A.spd.hex");
#[test]
fn peppy_memory_down() {
    for source in [
        HYNIX,
        include_str!("testdata/Hynix_HMT425S6CFR6A.spd.hex"),
        include_str!("testdata/Micron_4KTF25664HZ.spd.hex"),
        include_str!("testdata/Elpida_EDJ4216EFBG.spd.hex"),
    ] {
        let d = fixture(source);
        let mut raw = [[[0xff; 256]; 2]; 2];
        let t = collect(
            [[Slot::MemoryDown(&d), Slot::Empty]; 2],
            None,
            0,
            true,
            &mut raw,
        )
        .unwrap();
        assert_eq!(t.rankmap, [1, 1]);
        assert_eq!(t.channel_size_mib, [2048, 2048]);
        assert_eq!(t.lanes, 8);
        assert_eq!(raw[1], [d, [0; 256]]);
        let clock = Clock::candidates(&t, 0).next().unwrap();
        assert_eq!(
            (
                clock.tck,
                clock.mhz,
                clock.multiplier,
                clock.cas,
                clock.qclk_ps()
            ),
            (320, 800, 6, 11, 625)
        );
        let timing = Timings::from_spd(&t, clock, 0);
        assert_eq!(
            (
                timing.aa,
                timing.cwl,
                timing.wr,
                timing.rfc,
                timing.refi,
                timing.cmd
            ),
            (11, 8, 12, 208, 6240, 1)
        );
    }
}
#[test]
fn cas_holes_and_reference_clock_capability() {
    let mut d = fixture(HYNIX);
    d[12] = 9; // tCK = 1.125 ns; the first compatible 100 MHz ref bin is 800 MHz or below.
    d[34] = 0;
    d[16] = 80;
    d[35] = 0;
    d[14] = 0b1000_0000; // Only CL11, not ceil(10 ns / tCK).
    update_crc(&mut d);
    let mut t = peppy([[Slot::MemoryDown(&d), Slot::Empty]; 2]);
    let enabled = PCI_CAPID0_B::PLL_REF100_CFG.val(1).value;
    let c = Clock::candidates(&t, enabled).next().unwrap();
    assert_eq!((c.mhz, c.cas), (800, 11));
    t.dimms[0][0].as_mut().unwrap().tck_min_256ns = 280;
    t.dimms[1][0].as_mut().unwrap().tck_min_256ns = 280;
    assert_eq!(Clock::candidates(&t, enabled).next().unwrap().mhz, 900);
    assert_eq!(Clock::candidates(&t, 0).next().unwrap().mhz, 800);
    t.cas = 1 << 6;
    let slower = Clock::candidates(&t, enabled).next().unwrap();
    assert_eq!((slower.mhz, slower.cas), (533, 6));
}

#[test]
fn peppy_and_traditional_controller_programming() {
    use configure_mc::{Config, TrainingState};
    let d = fixture(HYNIX);
    let t = peppy([[Slot::MemoryDown(&d), Slot::Empty]; 2]);
    let clock = Clock::candidates(&t, 0).next().unwrap();
    for ult in [false, true] {
        // Aligned RAM backing for MmioWindow-backed MchBar writes. No electrical
        // behavior or broadcast aliasing is simulated by this test.
        let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
        let m = host_mch(&mut aperture);
        let mut tm = Timings::from_spd(&t, clock, 0);
        let mut state = TrainingState::default();
        configure_mc::configure(
            m,
            &t,
            clock,
            &mut tm,
            Config {
                ult,
                ..Config::default()
            },
            &mut state,
        )
        .unwrap();
        for ch in 0..2 {
            assert_eq!(m.read32(mc(0x4000, ch)), 0x1986716b);
            assert_eq!(m.read32(mc(0x4298, ch)), 0x6cd01860);
            assert_eq!(m.read32(mc(0x42a4, ch)), 0x41008200);
            assert_eq!(m.read32(mc(0x4290, ch)), if ult { 0x10080 } else { 0x4080 });
            assert_eq!(
                m.read32(0x1c20 + 0x100 * ch as u32),
                if ult { 0x31 } else { 1 }
            );
        }
        assert_eq!(m.read32(0x3600), 0x0200c040);
        assert_eq!(m.read32(0x3620), 0x03b08060);
        assert_eq!(m.read32(0x58a4), u32::from(ult));
        assert_eq!(m.read32(0x5004), if ult { 0x780008 } else { 0x680008 });
        let mr = ddr3::ModeRegisters::derive(&t, tm, ult);
        assert_eq!(mr.0[0][0], [0xd70, 0]);
        assert_eq!(mr.0[2][0][0], if ult { 0x498 } else { 0x298 });
        let mut regs = save_restore::TrainingRegisters::EMPTY;
        regs.capture(m);
        let saved = m.read32(0x429c);
        m.write32(0x429c, 0xdeadcafe);
        m.write32(0x4298, 0x55);
        // SAFETY: a local snapshot, replayed into its same RAM aperture.
        unsafe { regs.restore(m) };
        assert_eq!(m.read32(0x429c), saved);
        assert_eq!(m.read32(0x4298), 0x55); // regenerated, not captured
    }
}

#[test]
fn ecc_scrub_sets_its_own_mux_and_leaves_test_mode_on_error() {
    let mut d = fixture(HYNIX);
    d[8] |= 8;
    update_crc(&mut d);
    let t = peppy([[Slot::Empty; 2], [Slot::MemoryDown(&d), Slot::Empty]]);
    assert_eq!(t.lanes, 9);
    for error in [false, true] {
        let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
        let m = host_mch(&mut aperture);
        m.write32(0x4804, 2 << 16 | if error { 2 } else { 0 });
        let result = memory_scrub::ecc_scrub(m, &t, testing_io::Stepping::C0);
        assert_eq!(result, if error { Err(Error::Scrub) } else { Ok(()) });
        assert_eq!(m.read32(mc(0x4040, 1)), 0x8092); // 3 x LFSR plus ECC data source
        assert_eq!(m.read32(0x48a8), 0x3000);
        assert_eq!(m.read32(0x48b0), 0x3000);
        let stop = reut::read_qword(m, 0x48f0);
        assert_eq!(
            REUT_SEQ_BASE_ADDR::BANK_ADDR.read(stop),
            if error { 0 } else { 7 }
        );
        assert_eq!(REUT_SEQ_BASE_ADDR::COL_ADDR.read(stop), 127);
    }
}

#[test]
fn slot_one_centering_downloads_the_physical_test_rank() {
    use change_margin::{Margin, Param, RegFile, Target};
    let d = fixture(HYNIX);
    let t = peppy([[Slot::Empty, Slot::MemoryDown(&d)], [Slot::Empty; 2]]);
    let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
    let m = host_mch(&mut aperture);
    assert_eq!(testing_io::rank_channels(m, &t, 2), 1);
    assert_eq!(m.read32(0x4930), 2);
    let mut s = configure_mc::TrainingState::EMPTY;
    for (regfile, expected) in [
        (RegFile::Start, 2),
        (RegFile::Current, 2),
        (RegFile::Rank, 0),
    ] {
        Margin {
            param: Param::RdT,
            target: Target::All,
            rank: 0,
            regfile,
            commit: false,
        }
        .apply(m, &t, &mut s, 3)
        .unwrap();
        let ctrl = m.read32(0x3074);
        assert!(DDR_DATA_CONTROL_0::READ_RF_RD.is_set(ctrl));
        assert_eq!(DDR_DATA_CONTROL_0::READ_RF_RANK.read(ctrl), expected);
    }
}

#[test]
fn mpll_checks_idle_and_existing_lock_before_any_request() {
    let d = fixture(HYNIX);
    let mut t = peppy([[Slot::MemoryDown(&d), Slot::Empty], [Slot::Empty; 2]]);
    let dimm = t.dimms[0][0].as_mut().unwrap();
    dimm.tck_min_256ns = 274;
    dimm.taa_min_256ns = 11 * 274;
    let cap = PCI_CAPID0_B::PLL_REF100_CFG.val(1).value;
    let saved = Clock::candidates(&t, cap).find(|c| c.mhz == 700).unwrap();
    let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
    let m = host_mch(&mut aperture);
    m.write32(0x5084, 1 << 16);
    m.write32(0x5e04, 7);
    let init = |saved| timings::initialise_mpll(m, &t, cap, saved);
    let req = 0x4000_0007; // preserve sentinel on every locked/busy path
    m.write32(0x5e00, req);
    assert_eq!(init(Some(saved)), Err(Error::Pll));
    assert_eq!(init(None).unwrap().mhz, 933);
    assert_eq!(m.read32(0x5e00), req);
    m.write32(0x5e00, req | 16);
    assert_eq!(init(Some(saved)), Ok(saved));
    assert_eq!(m.read32(0x5e00), req | 16);
    m.write32(0x5e00, req | (1 << 31));
    assert_eq!(init(Some(saved)), Err(Error::PllBusy));
    assert_eq!(m.read32(0x5e00), req | (1 << 31));
    // An initially idle request that never completes is not a rejected bin.
    m.write32(0x5e00, 0);
    m.write32(0x5e04, 0);
    assert_eq!(init(Some(saved)), Err(Error::PllBusy));
    assert_eq!(m.read32(0x5e00), 0x8000_0017);
}

#[test]
fn candidates_require_representable_cas_cwl_and_trrd() {
    let d = fixture(HYNIX);
    let mut t = peppy([[Slot::MemoryDown(&d), Slot::Empty], [Slot::Empty; 2]]);
    t.cas = 1 << 17;
    assert!(Clock::candidates(&t, 0).next().is_none());
    t.cas = 1 << 6;
    t.dimms[0][0].as_mut().unwrap().taa_min_256ns = 320 * 6;
    assert_eq!(Clock::candidates(&t, 0).next().unwrap().mhz, 533);
    t.cas = 1 << 11;
    t.dimms[0][0].as_mut().unwrap().trrd_256ns = 320 * 16;
    let c = Clock::candidates(&t, 0).next().unwrap();
    assert_eq!((c.mhz, Timings::from_spd(&t, c, 0).rrd), (666, 14));
}

#[test]
fn peppy_single_channel_fuses_and_supply() {
    use PCI_CAPID0_A as cap;
    let mut d = fixture(HYNIX);
    let mut raw = [[[0xff; 256]; 2]; 2];
    let ch0 = [[Slot::MemoryDown(&d), Slot::Empty], [Slot::Empty; 2]];
    let t = collect(ch0, None, cap::PDCD::SET.value, true, &mut raw).unwrap();
    assert_eq!(t.rankmap, [1, 0]);
    assert_eq!(t.channel_size_mib, [2048, 0]);
    assert_eq!(raw[1], [[0; 256]; 2]);
    for (slots, fuse) in [
        ([ch0[0]; 2], cap::PDCD::SET.value),
        ([[Slot::MemoryDown(&d); 2], ch0[1]], cap::DDPCD::SET.value),
        (ch0, cap::DDRSZ.val(3).value),
        (ch0, cap::DDRSZ.val(2).value | cap::DDPCD::SET.value),
    ] {
        assert!(matches!(
            collect(slots, None, fuse, true, &mut raw),
            Err(Error::UnsupportedMemory)
        ));
    }
    for (volts, ult) in [(0, true), (3, false)] {
        d[6] = volts;
        update_crc(&mut d);
        let slots = [[Slot::MemoryDown(&d), Slot::Empty], [Slot::Empty; 2]];
        assert!(matches!(
            collect(slots, None, 0, ult, &mut raw),
            Err(Error::UnsupportedMemory)
        ));
        collect(slots, None, 0, !ult, &mut raw).unwrap();
    }
}

#[test]
fn crc_coverage_follows_spd_byte_zero() {
    for short in [false, true] {
        let mut d = fixture(HYNIX);
        d[0] = (d[0] & 127) | (u8::from(short) << 7);
        update_crc(&mut d);
        d[117] ^= 1;
        let parse = |d: &[u8; 256]| {
            collect(
                [[Slot::MemoryDown(d), Slot::Empty]; 2],
                None,
                0,
                true,
                &mut [[[0; 256]; 2]; 2],
            )
        };
        assert_eq!(parse(&d).is_ok(), short);
        d[116] ^= 1;
        assert!(matches!(parse(&d), Err(Error::SpdCrc)));
    }
}

#[test]
fn command_rejection_is_channel_local_and_exit_zq_errors_are_tolerated() {
    let d = fixture(HYNIX);
    let t = peppy([[Slot::MemoryDown(&d), Slot::Empty]; 2]);
    let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
    let m = host_mch(&mut aperture);
    for failed in [2, 1] {
        m.write32(0x4804, 3 << 16 | failed);
        let bad = ddr3::reset_and_program(
            m,
            &t,
            &ddr3::ModeRegisters::default(),
            true,
            &mut ddr3::ResetPin::SystemAgent,
        )
        .unwrap();
        assert_eq!(u32::from(bad), failed);
        for ch in 0..2 {
            let word = reut::read_qword(m, mc(0x41c0, ch));
            // Good channel reached ZQ_INIT; rejected channel stopped at MR2.
            assert_eq!(
                REUT_PAT_CADB_PROG::CMD.read(word),
                if failed & (1 << ch) == 0 { 6 } else { 0 }
            );
        }
        assert_eq!(ddr3::exit_selfrefresh(m, &t), Ok(()));
    }
    // An error ends the wait as in coreboot; exit ZQ_LONG errors only warn.
    m.write32(0x4804, 3);
    assert_eq!(
        reut::zq(m, &t, 1, reut::Calibration::Long),
        Err(Error::Reut)
    );
    assert_eq!(ddr3::exit_selfrefresh(m, &t), Ok(()));
    // Neither done nor error: the controller is stuck.
    m.write32(0x4804, 0);
    assert_eq!(ddr3::exit_selfrefresh(m, &t), Err(Error::Timeout));
}

#[test]
fn cadb_enable_is_independent_of_pattern_and_zq_reset_is_not_zqcs() {
    use testing_io::{Address, Commands, IoTest, Pattern, Stepping};
    let d = fixture(HYNIX);
    let t = peppy([[Slot::MemoryDown(&d), Slot::Empty], [Slot::Empty; 2]]);
    let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
    let m = host_mch(&mut aperture);
    m.write32(0x4804, 1 << 16);
    for (pattern, en_cadb, mux) in [(Pattern::Basic, true, 2), (Pattern::Cadb, false, 0)] {
        IoTest {
            commands: Commands::Read,
            cachelines: 2,
            loops: 7,
            address: Address::default(),
            stop_on_error: 0,
            wdb_start: 0,
            wdb_stop: 9,
            wdb_rate: 32,
            pattern,
            en_cadb,
            wait: 0,
        }
        .setup(m, &t, Stepping::C0, 1)
        .unwrap();
        assert_eq!(m.read8(0x4198), u8::from(en_cadb));
        assert_eq!(
            REUT_PAT_WDB_CL_MUX_CFG::MUX_0_CONTROL.read(m.read32(0x4040)),
            mux
        );
    }
    assert_eq!(REUT_PAT_CADB_MRS::DELAY_GAP.read(m.read32(0x419c)), 7);
    reut::zq(m, &t, 1, reut::Calibration::Reset).unwrap();
    assert_eq!(REUT_PAT_CADB_MRS::DELAY_GAP.read(m.read32(0x419c)), 1);
    assert_eq!(
        REUT_PAT_CADB_PROG::ADDR.read(reut::read_qword(m, 0x41c0)),
        0
    );
}

#[test]
fn common_cas_and_ecc_fuses() {
    let mut d = fixture(HYNIX);
    let mut raw = [[[0; 256]; 2]; 2];
    d[14] = 1 << 7;
    d[8] |= 8;
    update_crc(&mut d);
    let mut d2 = d;
    d2[14] = 1 << 6;
    d2[8] &= !8;
    update_crc(&mut d2);
    let slots = [
        [Slot::MemoryDown(&d), Slot::MemoryDown(&d2)],
        [Slot::Empty; 2],
    ];
    assert!(matches!(
        collect(slots, None, 0, true, &mut raw),
        Err(Error::UnsupportedMemory)
    ));
    d2[14] = d[14];
    update_crc(&mut d2);
    let slots = [
        [Slot::MemoryDown(&d), Slot::MemoryDown(&d2)],
        [Slot::Empty; 2],
    ];
    let t = collect(slots, None, 0, true, &mut raw).unwrap();
    assert_eq!(t.lanes, 8);
    assert_eq!(t.rankmap, [5, 0]);
    let forced = PCI_CAPID0_A::FDEE::SET.value;
    let disabled = PCI_CAPID0_A::ECCDIS::SET.value;
    assert!(matches!(
        collect(slots, None, forced, true, &mut raw),
        Err(Error::UnsupportedMemory)
    ));
    let slots = [[Slot::MemoryDown(&d), Slot::Empty]; 2];
    assert_eq!(
        collect(slots, None, forced | disabled, true, &mut raw)
            .unwrap()
            .lanes,
        9
    );
    assert_eq!(
        collect(slots, None, disabled, true, &mut raw)
            .unwrap()
            .lanes,
        8
    );
}

#[test]
fn ggc_value_sets_ivd_when_igd_disabled() {
    assert_eq!(memory_map::ggc_value(true), 0x210);
    assert_eq!(memory_map::ggc_value(false), 0x002);
    assert_ne!(memory_map::ggc_value(false) & memory_map::GGC_IVD, 0);
    assert_eq!(memory_map::ggc_value(true) & memory_map::GGC_IVD, 0);
}

#[test]
fn gdxc_clear_and_lock_clears_trace_regs_and_locks_ocla() {
    let mut backing = [0xffff_ffffu32; (memory_map::GDXCBAR_SIZE as usize) / 4];
    // SAFETY: `backing` is a 4-byte-aligned 4 KiB RAM buffer owned by the test.
    let win = unsafe {
        fstart_core::mmio::MmioWindow::new(backing.as_mut_ptr() as usize, memory_map::GDXCBAR_SIZE)
    };
    memory_map::clear_and_lock_gdxc(win);
    for off in [0x10u32, 0x14, 0x18, 0x20, 0x28] {
        assert_eq!(win.read32(off), 0, "GDXC offset {off:#x} should be cleared");
    }
    // MPCOHTRK_GDXC_OCLA_ADDRESS_HI has only LOCK (bit 2) set.
    assert_eq!(win.read32(0x24), 1 << 2);
    // Unrelated GDXC registers remain untouched.
    assert_eq!(win.read32(0x00), 0xffff_ffff);
    assert_eq!(win.read32(0x2c), 0xffff_ffff);
}

#[test]
fn memory_map_without_igd_and_invalid_inputs() {
    use memory_map::{MemoryMap, Reservations};
    let r = Reservations {
        tseg: 8,
        dpr: 4,
        me: 0,
        igd_primary: false,
    };
    let m = MemoryMap::compute(4096, r, false).unwrap();
    assert_eq!(
        (m.dpr_base, m.tseg, m.gtt, m.bdsm, m.tolud),
        (2036, 2040, 2048, 2048, 2048)
    );
    assert_eq!(m.remap, Some((4096, 6143)));
    assert_eq!(m.touud, 6144);
    // Non-power-of-two TSEG, DPR > 255, ME > TOM, and TOM > 512 GiB fail cleanly.
    assert_eq!(
        MemoryMap::compute(4096, Reservations { tseg: 6, ..r }, false),
        Err(Error::InvalidMap)
    );
    assert_eq!(
        MemoryMap::compute(4096, Reservations { dpr: 256, ..r }, false),
        Err(Error::InvalidMap)
    );
    assert_eq!(
        MemoryMap::compute(16, Reservations { me: 32, ..r }, false),
        Err(Error::InvalidMap)
    );
    assert_eq!(
        MemoryMap::compute(0x80001, r, false),
        Err(Error::InvalidMap)
    );
}

#[test]
fn mchbar_accessors_rcomp_and_io_reset_on_host_buffer() {
    let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
    let m = host_mch(&mut aperture);
    assert_eq!(m.window().size() as usize, crate::haswell::MCHBAR_SIZE);

    // 8/16/32/64-bit accessors and RMW helpers.
    m.write8(0x10, 0x5a);
    assert_eq!(m.read8(0x10), 0x5a);
    m.write16(0x12, 0xcafe);
    assert_eq!(m.read16(0x12), 0xcafe);
    m.write32(0x14, 0x1234_5678);
    m.setbits32(0x14, 0x0000_0007);
    assert_eq!(m.read32(0x14), 0x1234_567f);
    m.clrbits32(0x14, 0x0000_000f);
    assert_eq!(m.read32(0x14), 0x1234_5670);
    m.clrsetbits32(0x14, 0x0000_ff00, 0x0000_ab00);
    assert_eq!(m.read32(0x14), 0x1234_ab70);
    m.write64(0x20, 0x0123_4567_89ab_cdef);
    assert_eq!(m.read64(0x20), 0x0123_4567_89ab_cdef);

    // wait_for_first_rcomp: times out when bit 16 is clear, succeeds when set.
    assert_eq!(wait_for_first_rcomp_timeout(m, 1), Err(Error::Timeout));
    m.write32(0x5084, 1 << 16);
    assert_eq!(wait_for_first_rcomp(m), Ok(()));

    // io_reset: sets RESET_IO preserving other bits in 0x5030, succeeds once cleared.
    let reset = MC_INIT_STATE_G::RESET_IO::SET.value;
    m.write32(0x5030, 0x0000_0081);
    let res = io_reset_with(m, |done| {
        assert!(!done());
        assert_eq!(m.read32(0x5030), 0x0000_0081 | reset);
        // Simulate hardware self-clearing RESET_IO.
        m.clrbits32(0x5030, reset);
        assert!(done());
        Ok(())
    });
    assert_eq!(res, Ok(()));
    assert_eq!(m.read32(0x5030), 0x0000_0081);

    // io_reset timeout path when RESET_IO remains stuck.
    assert_eq!(io_reset_with(m, |done| poll(done, 1)), Err(Error::Timeout));
    assert_eq!(m.read32(0x5030), 0x0000_0081 | reset);
}

#[test]
fn save_restore_round_trips_every_training_register() {
    let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
    let m = host_mch(&mut aperture);
    let offsets = save_restore::offsets();

    // Seed every saved register with a unique deterministic pattern.
    for (i, &off) in offsets.iter().enumerate() {
        m.write32(off, 0xa500_0000 | (i as u32) | (off << 12));
    }
    // Also mark a non-saved register (0x4298 is regenerated, not saved).
    m.write32(0x4298, 0x1122_3344);

    let mut saved = save_restore::TrainingRegisters::EMPTY;
    saved.capture(m);

    // Corrupt every saved register and update the non-saved register.
    for &off in &offsets {
        m.write32(off, 0xdead_beef);
    }
    m.write32(0x4298, 0x5566_7788);

    // SAFETY: replaying a snapshot captured from this same host test buffer.
    unsafe { saved.restore(m) };

    for (i, &off) in offsets.iter().enumerate() {
        let expected = 0xa500_0000 | (i as u32) | (off << 12);
        assert_eq!(m.read32(off), expected, "mismatch at offset {off:#x}");
    }
    assert_eq!(m.read32(0x4298), 0x5566_7788);
}

#[test]
fn wdb_and_reut_program_host_mchbar_registers() {
    let d = fixture(HYNIX);
    let mut t = peppy([[Slot::MemoryDown(&d), Slot::Empty]; 2]);
    let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
    let m = host_mch(&mut aperture);

    // WDB setup programs LFSR seeds, LMN mux config, and clears LDAT SDAT.
    wdb::setup(m, &t);
    let expected_lmn = (REUT_PAT_CL_MUX_LMN::EN_SWEEP_FREQ::SET
        + REUT_PAT_CL_MUX_LMN::L_COUNTER.val(1)
        + REUT_PAT_CL_MUX_LMN::M_COUNTER.val(1)
        + REUT_PAT_CL_MUX_LMN::N_COUNTER.val(10))
    .value;
    for ch in 0..2 {
        assert_eq!(m.read32(mc(0x42d4, ch)), 0);
        assert_eq!(m.read32(mc(0x4078, ch)), expected_lmn);
        assert_eq!(m.read32(mc(0x4054, ch)), 0xa10ca1);
        assert_eq!(m.read32(mc(0x4058, ch)), 0xef0d08);
        assert_eq!(m.read32(mc(0x405c, ch)), 0xad0a1e);
        assert_eq!(m.read32(mc(0x4048, ch)), 0xa10ca1);
    }

    // Cleanup LFSR uses fixed cleanup seeds; fixed_pattern writes LDAT line.
    wdb::program_lfsr(m, &t, true);
    assert_eq!(m.read32(mc(0x4054, 0)), 0xaaaaaa);
    assert_eq!(m.read32(mc(0x4058, 0)), 0xcccccc);
    assert_eq!(m.read32(mc(0x405c, 0)), 0xf0f0f0);
    wdb::fixed_pattern(m, &t, &[0xff, 0x00], &[0, 1], 3);
    assert_eq!(m.read32(mc(0x42dc, 0)), 0x5555_5555);
    assert_eq!(m.read32(mc(0x42e0, 0)), 0x5555_5555);
    assert_eq!(
        m.read32(mc(0x42d0, 0)),
        (LDAT_PDAT::FAST_ADDR.val(3) + LDAT_PDAT::CMD_B.val(8)).value
    );

    // REUT MRS with address mirroring on channel 1 rank 0, plus bitlane_errors.
    t.mirrored[1] = 1;
    m.write32(0x4804, 3 << 16);
    reut::mrs(m, &t, 0, 1, 1, 0x00a8).unwrap();
    reut::mrs(m, &t, 1, 1, 1, 0x00a8).unwrap();
    let ch0_word = m.read64(mc(0x41c0, 0));
    let ch1_word = m.read64(mc(0x41c0, 1));
    assert_eq!(REUT_PAT_CADB_PROG::ADDR.read(ch0_word), 0x00a8);
    assert_eq!(REUT_PAT_CADB_PROG::BANK.read(ch0_word), 1);
    assert_eq!(REUT_PAT_CADB_PROG::ADDR.read(ch1_word), 0x0150);
    assert_eq!(REUT_PAT_CADB_PROG::BANK.read(ch1_word), 2);

    m.write32(mc(0x40e0, 0), 0x0403_0201);
    m.write32(mc(0x40e4, 0), 0x0807_0605);
    m.write8(mc(0x40e8, 0), 0x09);
    assert_eq!(reut::bitlane_errors(m, 0), [1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[test]
fn mrc_cache_serialization_invalidation_and_boot_mode_matrix() {
    use zerocopy::IntoBytes;

    // 1. BootMode selection matrix: S3 vs Warm vs Fast vs Cold.
    let s3_ready = BootState::from_pmcon(0xa0, true, false);
    assert_eq!(s3_ready.select(true, false), Ok(BootMode::S3));
    assert_eq!(s3_ready.select(true, true), Ok(BootMode::S3));
    assert_eq!(
        s3_ready.select(false, false),
        Err(Error::ResumeRequiresReset)
    );
    assert_eq!(
        BootState::from_pmcon(0xa0, true, true).select(true, false),
        Err(Error::ResumeRequiresReset)
    );
    assert_eq!(
        BootState::from_pmcon(0x20, true, false).select(true, false),
        Err(Error::ResumeRequiresReset)
    );

    // Non-S3 with self-refresh: Warm if cache valid & not early_reset/cpu_replaced.
    let warm = BootState::from_pmcon(0xa0, false, false);
    assert_eq!(warm.select(true, false), Ok(BootMode::Warm));
    assert_eq!(warm.select(true, true), Ok(BootMode::Cold));
    assert_eq!(warm.select(false, false), Ok(BootMode::Cold));
    assert_eq!(
        BootState::from_pmcon(0xa0, false, true).select(true, false),
        Ok(BootMode::Cold)
    );

    // Non-S3 without self-refresh: Fast if cache valid, Cold otherwise.
    let fast = BootState::from_pmcon(0x00, false, false);
    assert_eq!(fast.select(true, false), Ok(BootMode::Fast));
    assert_eq!(fast.select(true, true), Ok(BootMode::Fast));
    assert_eq!(fast.select(false, false), Ok(BootMode::Cold));
    assert_eq!(
        BootState::from_pmcon(0x00, false, true).select(true, false),
        Ok(BootMode::Cold)
    );

    // 2. Cache frame serialization and invalidation checks.
    let d = fixture(HYNIX);
    let mut frame = flow::Frame::EMPTY;
    let t = collect(
        [[Slot::MemoryDown(&d), Slot::Empty]; 2],
        None,
        0,
        true,
        &mut frame.spd,
    )
    .unwrap();
    frame.magic = *b"HSW2";
    frame.key = [0x42; 32];
    frame.tck = 320;

    let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
    let m = host_mch(&mut aperture);
    m.write32(0x4000, 0x1986_716b);
    frame.regs.capture(m);

    let mut blob = [0u8; CACHE_BYTES];
    blob.copy_from_slice(frame.as_bytes());
    let (clock, regs_bytes) = frame.accept(&blob, &t, 0).unwrap();
    assert_eq!(clock.mhz, 800);
    assert_eq!(regs_bytes.len(), save_restore::WORDS * 4);

    // Restoring serialized register payload recovers captured MMIO state.
    let mut replayed = save_restore::TrainingRegisters::EMPTY;
    replayed.as_mut_bytes().copy_from_slice(regs_bytes);
    m.write32(0x4000, 0);
    // SAFETY: replaying into the same host RAM test buffer.
    unsafe { replayed.restore(m) };
    assert_eq!(m.read32(0x4000), 0x1986_716b);

    // Wrong length (truncated or oversized) is rejected.
    assert!(frame.accept(&blob[..CACHE_BYTES - 1], &t, 0).is_none());
    let mut oversized = [0u8; CACHE_BYTES + 1];
    oversized[..CACHE_BYTES].copy_from_slice(&blob);
    assert!(frame.accept(&oversized, &t, 0).is_none());

    // Mutating magic, key, SPD, or tCK invalidates the cache.
    for off in [
        core::mem::offset_of!(flow::Frame, magic),
        core::mem::offset_of!(flow::Frame, key) + 15,
        core::mem::offset_of!(flow::Frame, spd) + 12,
        core::mem::offset_of!(flow::Frame, tck),
    ] {
        blob[off] ^= 0x01;
        assert!(
            frame.accept(&blob, &t, 0).is_none(),
            "cache should reject mutation at byte {off}"
        );
        blob[off] ^= 0x01;
    }
    // Mutating saved register bytes alone stays structurally accepted (payload integrity
    // is verified by the persistent store's MAC/checksum layer before raminit).
    assert!(frame.accept(&blob, &t, 0).is_some());
}
