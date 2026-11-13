//! CPGC: the converged pattern generator and checker that drives DRAM
//! traffic for training, IOSF port 0x0d.

use super::dunit::{DCO, DCO_REG};
use crate::baytrail::iosf::{CPGC, Iosf, IosfReg};
use fstart_core::services::ServiceError;
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

register_bitfields![u32,
    pub CMD_CTL [
        START_DELAY OFFSET(0) NUMBITS(8) [],
        /// Start the test; reads as zero.
        START_TEST OFFSET(8) NUMBITS(1) [],
        /// Force the test to exit; reads as zero.
        STOP_TEST OFFSET(9) NUMBITS(1) [],
        GLB_START OFFSET(10) NUMBITS(1) [],
        GLB_STOP OFFSET(11) NUMBITS(1) [],
        INIT_MODE OFFSET(12) NUMBITS(2) [
            Idle = 0,
            Test = 1,
            ErrorClear = 2
        ],
        /// Loop count: 2^(n-1) iterations, 0 = infinite.
        EXP_LOOP_CNT OFFSET(16) NUMBITS(4) [],
        STOP_ON_WRAP OFFSET(20) NUMBITS(1) [],
        SEQ_STRT_PTR OFFSET(24) NUMBITS(2) [],
        SEQ_END_PTR OFFSET(28) NUMBITS(2) []
    ],
    pub TESTSTAT [
        LOOP_CNT OFFSET(0) NUMBITS(16) [],
        SUBSEQ_PTR OFFSET(24) NUMBITS(2) [],
        TEST_BUSY OFFSET(30) NUMBITS(1) [],
        TEST_DONE OFFSET(31) NUMBITS(1) []
    ],
    pub SEQCTL [
        /// Burst count: 2^(n-1) CAS commands.
        NUM_BURSTS OFFSET(0) NUMBITS(4) [],
        COMP_ON_WRAP OFFSET(4) NUMBITS(1) [],
        WAIT_REF OFFSET(5) NUMBITS(1) [],
        SUBSEQ_WAIT OFFSET(8) NUMBITS(8) [],
        /// 0 = 32 B, 1 = 64 B requests.
        REQ_DATA_SIZE OFFSET(16) NUMBITS(1) [],
        SUBSEQ_TYPE OFFSET(20) NUMBITS(4) [
            Read = 0x0,
            Write = 0x1,
            ReadRefresh = 0x8,
            WriteRefresh = 0x9
        ],
        RESET_ADDR OFFSET(24) NUMBITS(1) []
    ],
    pub SEQVAR [ VAR_HI_SHIFT OFFSET(25) NUMBITS(3) [] ],
    pub DPAT_MUXCTL [
        UNISEQ0_MODE OFFSET(0) NUMBITS(2) [],
        UNISEQ1_MODE OFFSET(2) NUMBITS(2) [],
        UNISEQ2_MODE OFFSET(4) NUMBITS(2) [],
        ECC_DISABLE OFFSET(23) NUMBITS(1) [],
        RELOAD_RATE OFFSET(24) NUMBITS(3) [],
        RELOAD_EN OFFSET(31) NUMBITS(1) []
    ],
    pub DPAT_INVDCCTL [
        ECC_INV_MASK OFFSET(0) NUMBITS(8) [],
        ECC_INDEP_SR OFFSET(23) NUMBITS(1) [],
        ROTATE_RATE OFFSET(24) NUMBITS(3) [],
        DC_POL OFFSET(29) NUMBITS(1) [],
        DC_OR_INV OFFSET(30) NUMBITS(1) [],
        ROTATE_EN OFFSET(31) NUMBITS(1) []
    ],
    pub ERR_CTL [
        STOP_ON_N OFFSET(0) NUMBITS(6) [],
        CLR_ALL_ERR OFFSET(8) NUMBITS(1) [],
        INJ_ERR OFFSET(9) NUMBITS(1) [],
        STOP_ON_ERROR OFFSET(12) NUMBITS(2) [],
        CHUNK_CHK_EN OFFSET(16) NUMBITS(8) [],
        BURST_CHK_EN OFFSET(24) NUMBITS(8) []
    ],
    pub ERR_XSTAT [
        ECC_LANE_ERR OFFSET(0) NUMBITS(8) [],
        BYTEGRP_ERR OFFSET(8) NUMBITS(8) [],
        ECCGRP_ERR OFFSET(16) NUMBITS(1) [],
        CHUNK_ERR OFFSET(24) NUMBITS(8) []
    ]
];

const CMD_CTL_REG: IosfReg<CMD_CTL::Register> = CPGC.reg(0x11);
const TESTSTAT_REG: IosfReg<TESTSTAT::Register> = CPGC.reg(0x12);
const SEQ0CTL: IosfReg<SEQCTL::Register> = CPGC.reg(0x20);
const SEQ0VAR: IosfReg<SEQVAR::Register> = CPGC.reg(0x21);
const SEQ0FIX: IosfReg = CPGC.reg(0x22);
const SEQ1CTL: IosfReg<SEQCTL::Register> = CPGC.reg(0x23);
const SEQ1VAR: IosfReg<SEQVAR::Register> = CPGC.reg(0x24);
const SEQ1FIX: IosfReg = CPGC.reg(0x25);
const DPAT_BUFCTL: IosfReg = CPGC.reg(0x61);
const DPAT_MUXCTL_REG: IosfReg<DPAT_MUXCTL::Register> = CPGC.reg(0x62);
const DPAT_UNISEQ: [IosfReg; 3] = [CPGC.reg(0x63), CPGC.reg(0x64), CPGC.reg(0x65)];
const DPAT_INVDCCTL_REG: IosfReg<DPAT_INVDCCTL::Register> = CPGC.reg(0x66);
const DPAT_EXTBUF0: IosfReg = CPGC.reg(0x80);
const ERR_CTL_REG: IosfReg<ERR_CTL::Register> = CPGC.reg(0xa1);
const ERR_LANE_MASK_LO: IosfReg = CPGC.reg(0xa2);
const ERR_LANE_MASK_HI: IosfReg = CPGC.reg(0xa3);
const ERR_LANE_XMASK: IosfReg = CPGC.reg(0xa4);
const ERR_XSTAT_REG: IosfReg<ERR_XSTAT::Register> = CPGC.reg(0xa7);

/// Which subsequences a test runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Test {
    /// Subsequence 1 only: read back (receive enable sampling).
    SingleRead,
    /// Subsequence 0 only: write (write-leveling sampling).
    SingleWrite,
    /// Both: write then read `bursts` bursts and compare.
    WriteRead { bursts: u32 },
}

/// The CPGC engine.
pub struct Cpgc<'a, P> {
    iosf: &'a Iosf<P>,
}

impl<'a, P: ConfigRegionAccess> Cpgc<'a, P> {
    pub fn new(iosf: &'a Iosf<P>) -> Self {
        Self { iosf }
    }

    /// Hand the D-unit's PMI to the CPGC.
    pub fn enable(&self) {
        self.iosf
            .reg(DCO_REG)
            .modify(DCO::PMICTL::SET + DCO::PMIDIS::CLEAR);
    }

    pub fn disable(&self) {
        self.iosf
            .reg(DCO_REG)
            .modify(DCO::PMICTL::CLEAR + DCO::PMIDIS::CLEAR);
    }

    fn setup_common(&self) {
        let iosf = self.iosf;
        // Stop any previous test.
        iosf.reg(CMD_CTL_REG).modify(CMD_CTL::STOP_TEST::SET);
        // Check all DQ lanes, chunks and bursts; clear old errors.
        iosf.reg(ERR_CTL_REG).write(
            ERR_CTL::CHUNK_CHK_EN.val(0xff)
                + ERR_CTL::BURST_CHK_EN.val(0xff)
                + ERR_CTL::CLR_ALL_ERR::SET,
        );
        iosf.reg(ERR_LANE_MASK_LO).set(0xffff_ffff);
        iosf.reg(ERR_LANE_MASK_HI).set(0xffff_ffff);
        iosf.reg(ERR_LANE_XMASK).set(0);
        // LFSR seeds.
        for (reg, seed) in DPAT_UNISEQ
            .into_iter()
            .zip([0xf0f0_f0f0, 0xcccc_cccc, 0xaaaa_aaaa])
        {
            iosf.reg(reg).set(seed);
        }
        const PATTERNS: [u32; 8] = [0xaa, 0xcc, 0xf0, 0x33, 0x55, 0x0f, 0xc0, 0x03];
        for (i, pattern) in PATTERNS.iter().chain(PATTERNS.iter()).enumerate() {
            iosf.reg(DPAT_EXTBUF0.offset(i as u32)).set(*pattern);
        }
        iosf.reg(DPAT_MUXCTL_REG).write(
            DPAT_MUXCTL::UNISEQ0_MODE.val(2)
                + DPAT_MUXCTL::UNISEQ1_MODE.val(2)
                + DPAT_MUXCTL::UNISEQ2_MODE.val(2)
                + DPAT_MUXCTL::ECC_DISABLE::SET,
        );
        iosf.reg(DPAT_BUFCTL).set(0);
        iosf.reg(DPAT_INVDCCTL_REG)
            .write(DPAT_INVDCCTL::ROTATE_EN::SET + DPAT_INVDCCTL::ROTATE_RATE.val(3));
    }

    /// Program the subsequences and the command control for `test`. The
    /// engine is not started.
    pub fn setup(&self, test: Test) {
        self.setup_common();
        let bursts = match test {
            Test::WriteRead { bursts } => bursts.max(1).ilog2(),
            Test::SingleRead | Test::SingleWrite => 1,
        };
        let seq = |kind| {
            SEQCTL::NUM_BURSTS.val(bursts)
                + kind
                + SEQCTL::RESET_ADDR::SET
                + SEQCTL::REQ_DATA_SIZE::SET
                + SEQCTL::WAIT_REF::SET
        };
        self.iosf
            .reg(SEQ0CTL)
            .write(seq(SEQCTL::SUBSEQ_TYPE::WriteRefresh));
        self.iosf
            .reg(SEQ1CTL)
            .write(seq(SEQCTL::SUBSEQ_TYPE::ReadRefresh));
        self.iosf.reg(SEQ0VAR).write(SEQVAR::VAR_HI_SHIFT.val(3));
        self.iosf.reg(SEQ1VAR).write(SEQVAR::VAR_HI_SHIFT.val(3));
        self.iosf.reg(SEQ0FIX).set(0);
        self.iosf.reg(SEQ1FIX).set(0);
        let (first, last) = match test {
            Test::SingleRead => (1, 1),
            Test::SingleWrite => (0, 0),
            Test::WriteRead { .. } => (0, 1),
        };
        self.iosf.reg(CMD_CTL_REG).write(
            CMD_CTL::INIT_MODE::Test
                + CMD_CTL::EXP_LOOP_CNT.val(1)
                + CMD_CTL::SEQ_STRT_PTR.val(first)
                + CMD_CTL::SEQ_END_PTR.val(last),
        );
    }

    pub fn clear_errors(&self) {
        self.iosf.reg(ERR_CTL_REG).modify(ERR_CTL::CLR_ALL_ERR::SET);
    }

    pub fn start(&self) {
        self.iosf
            .reg(CMD_CTL_REG)
            .modify(CMD_CTL::STOP_TEST::CLEAR + CMD_CTL::START_TEST::SET);
    }

    /// Wait for the test to complete. `Err` when the engine stopped
    /// without completing or did not finish in time.
    pub fn poll(&self) -> Result<(), ServiceError> {
        for _ in 0..100_000 {
            let status = self.iosf.reg(TESTSTAT_REG).extract();
            if status.is_set(TESTSTAT::TEST_DONE) {
                return Ok(());
            }
            if !status.is_set(TESTSTAT::TEST_BUSY) {
                return Err(ServiceError::HardwareError);
            }
        }
        Err(ServiceError::Timeout)
    }

    pub fn stop(&self) {
        self.iosf.reg(CMD_CTL_REG).modify(CMD_CTL::STOP_TEST::SET);
    }

    /// Clear errors, run the programmed test to completion and stop.
    pub fn run(&self) -> Result<(), ServiceError> {
        self.clear_errors();
        self.start();
        let done = self.poll();
        self.stop();
        done
    }

    /// Byte groups that failed the last test, one bit per strobe.
    pub fn errors(&self) -> u8 {
        self.iosf.reg(ERR_XSTAT_REG).read(ERR_XSTAT::BYTEGRP_ERR) as u8
    }
}
