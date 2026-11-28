//! Sandy Bridge / Ivy Bridge system-agent register maps: host-bridge PCI
//! configuration, MCHBAR (memory controller, IOSAV, power management) and
//! DMIBAR. Offsets and field layouts follow coreboot
//! `northbridge/intel/sandybridge/registers/*.h` and `raminit_common.h`.

use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use fstart_pci::pci_type0_config;
use tock_registers::{register_bitfields, register_structs};

pci_type0_config! {
    /// Host bridge, PCI 00:00.0.
    pub struct HostBridgeConfig {
        (0x40 => pub epbar: MmioReadWrite<u32>),
        (0x44 => pub epbar_hi: MmioReadWrite<u32>),
        (0x48 => pub mchbar: MmioReadWrite<u32>),
        (0x4c => pub mchbar_hi: MmioReadWrite<u32>),
        (0x50 => pub ggc: MmioReadWrite<u16, GGC::Register>),
        (0x52 => _rsvd0),
        (0x54 => pub deven: MmioReadWrite<u32, DEVEN::Register>),
        (0x58 => pub pavpc: MmioReadWrite<u32>),
        (0x5c => pub dpr: MmioReadWrite<u32>),
        (0x60 => pub pciexbar: MmioReadWrite<u32, PCIEXBAR::Register>),
        (0x64 => pub pciexbar_hi: MmioReadWrite<u32>),
        (0x68 => pub dmibar: MmioReadWrite<u32>),
        (0x6c => pub dmibar_hi: MmioReadWrite<u32>),
        (0x70 => pub meseg_base: MmioReadWrite<u32>),
        (0x74 => pub meseg_base_hi: MmioReadWrite<u32>),
        (0x78 => pub meseg_mask: MmioReadWrite<u32, MESEG_MASK::Register>),
        (0x7c => pub meseg_mask_hi: MmioReadWrite<u32>),
        (0x80 => pub pam: [MmioReadWrite<u8>; 7]),
        (0x87 => pub lac: MmioReadWrite<u8>),
        (0x88 => pub smram: MmioReadWrite<u8>),
        (0x89 => _rsvd1),
        (0x90 => pub remapbase: MmioReadWrite<u32>),
        (0x94 => pub remapbase_hi: MmioReadWrite<u32>),
        (0x98 => pub remaplimit: MmioReadWrite<u32>),
        (0x9c => pub remaplimit_hi: MmioReadWrite<u32>),
        (0xa0 => pub tom: MmioReadWrite<u32>),
        (0xa4 => pub tom_hi: MmioReadWrite<u32>),
        (0xa8 => pub touud: MmioReadWrite<u32>),
        (0xac => pub touud_hi: MmioReadWrite<u32>),
        (0xb0 => pub bdsm: MmioReadWrite<u32>),
        (0xb4 => pub bgsm: MmioReadWrite<u32>),
        (0xb8 => pub tsegmb: MmioReadWrite<u32>),
        (0xbc => pub tolud: MmioReadWrite<u32>),
        (0xc0 => _rsvd2),
        (0xdc => pub skpad: MmioReadWrite<u32>),
        (0xe0 => _rsvd3),
        (0xe4 => pub capid0_a: MmioReadOnly<u32, CAPID0_A::Register>),
        (0xe8 => pub capid0_b: MmioReadOnly<u32, CAPID0_B::Register>),
        (0xec => _rsvd4),
        (0xf3 => pub didor: MmioReadWrite<u8>),
        (0xf4 => _rsvd5),
        (0x100 => @END),
    }
}

register_bitfields![u16,
    pub GGC [
        LOCK OFFSET(0) NUMBITS(1) [],
        /// IGD VGA disable.
        IVD OFFSET(1) NUMBITS(1) [],
        /// Graphics mode select: stolen memory in 32 MiB units minus one.
        GMS OFFSET(3) NUMBITS(5) [],
        /// GTT stolen memory size in MiB.
        GGMS OFFSET(8) NUMBITS(2) []
    ]
];

register_bitfields![u32,
    pub DEVEN [
        HOST OFFSET(0) NUMBITS(1) [],
        PEG12 OFFSET(1) NUMBITS(1) [],
        PEG11 OFFSET(2) NUMBITS(1) [],
        PEG10 OFFSET(3) NUMBITS(1) [],
        IGD OFFSET(4) NUMBITS(1) [],
        D4 OFFSET(7) NUMBITS(1) [],
        PEG60 OFFSET(13) NUMBITS(1) [],
        D7 OFFSET(14) NUMBITS(1) []
    ],
    /// PCIEXBAR (0x60, 32 bits of 64): the ECAM window.
    pub PCIEXBAR [
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// 0 = 256, 1 = 128, 2 = 64 buses.
        LENGTH OFFSET(1) NUMBITS(2) [],
        /// Base address bits 31:26.
        ADDRESS OFFSET(26) NUMBITS(6) []
    ],
    pub MESEG_MASK [
        LOCK OFFSET(10) NUMBITS(1) [],
        STOLEN_EN OFFSET(11) NUMBITS(1) [],
        MASK OFFSET(20) NUMBITS(12) []
    ],
    pub CAPID0_A [
        /// SNB DDR3 maximum frequency cap (DMFC).
        DMFC OFFSET(0) NUMBITS(3) [],
        WRTVREF OFFSET(1) NUMBITS(1) [],
        DIDOE OFFSET(10) NUMBITS(1) [],
        PDCD OFFSET(12) NUMBITS(1) [],
        DDPCD OFFSET(14) NUMBITS(1) [],
        DDRSZ OFFSET(19) NUMBITS(2) [],
        VTDD OFFSET(23) NUMBITS(1) [],
        ECC_FORCED OFFSET(24) NUMBITS(1) [],
        ECCDIS OFFSET(25) NUMBITS(1) []
    ],
    pub CAPID0_B [
        /// IVB DDR3 maximum frequency cap.
        DMFC OFFSET(4) NUMBITS(3) [],
        PLL_REF100 OFFSET(21) NUMBITS(3) []
    ]
];

register_bitfields![u32,
    /// GDCRRX: per-lane, per-rank receive timings.
    pub GDCR_RX [
        RCVEN_PI_CODE OFFSET(0) NUMBITS(6) [],
        RX_DQS_P_PI_CODE OFFSET(8) NUMBITS(7) [],
        RCVEN_LOGIC_DELAY OFFSET(16) NUMBITS(3) [],
        RX_DQS_N_PI_CODE OFFSET(20) NUMBITS(7) []
    ],
    /// GDCRTX: per-lane, per-rank transmit timings.
    pub GDCR_TX [
        TX_DQ_PI_CODE OFFSET(0) NUMBITS(6) [],
        TX_DQS_PI_CODE OFFSET(8) NUMBITS(6) [],
        TX_DQS_LOGIC_DELAY OFFSET(15) NUMBITS(3) [],
        TX_DQ_LOGIC_DELAY OFFSET(19) NUMBITS(1) []
    ],
    /// GDCRCMDPICODING: command/control phase interpolators.
    pub CMD_PI_CODING [
        CMD_PI_CODE OFFSET(0) NUMBITS(6) [],
        CTL_PI_CODE_D0 OFFSET(6) NUMBITS(6) [],
        CMD_LOGIC_DELAY OFFSET(12) NUMBITS(1) [],
        CMD_PHASE_DELAY OFFSET(13) NUMBITS(1) [],
        CMD_XOVER_ENABLE OFFSET(14) NUMBITS(1) [],
        CTL_LOGIC_DELAY_D0 OFFSET(15) NUMBITS(1) [],
        CTL_PHASE_DELAY_D0 OFFSET(16) NUMBITS(1) [],
        CTL_XOVER_ENABLE_D0 OFFSET(17) NUMBITS(1) [],
        CTL_PI_CODE_D1 OFFSET(18) NUMBITS(6) [],
        CTL_LOGIC_DELAY_D1 OFFSET(24) NUMBITS(1) [],
        CTL_PHASE_DELAY_D1 OFFSET(25) NUMBITS(1) [],
        CTL_XOVER_ENABLE_D1 OFFSET(26) NUMBITS(1) []
    ],
    /// GDCRTRAININGMOD: data training mode control.
    pub TRAINING_MOD [
        RECEIVE_ENABLE_MODE OFFSET(0) NUMBITS(1) [],
        WRITE_LEVELING_MODE OFFSET(1) NUMBITS(1) [],
        TRAINING_RANK_SEL OFFSET(2) NUMBITS(2) [],
        ENABLE_DQS_WL OFFSET(4) NUMBITS(4) [],
        DQS_LOGIC_DELAY_WL OFFSET(8) NUMBITS(1) [],
        DQ_DQS_TRAINING_RES OFFSET(9) NUMBITS(1) [],
        DELAY_DQ OFFSET(14) NUMBITS(1) [],
        ODT_ALWAYS_ON OFFSET(15) NUMBITS(1) [],
        FORCE_DRIVE_ENABLE OFFSET(20) NUMBITS(1) [],
        DFT_TX_PI_CLK_VIEW OFFSET(21) NUMBITS(1) [],
        DFT_TX_PI_CLK_SWAP OFFSET(22) NUMBITS(1) [],
        EARLY_ODT_EN OFFSET(23) NUMBITS(1) [],
        VREF_GEN_CTL OFFSET(24) NUMBITS(6) [],
        EXT_VREF_SEL OFFSET(30) NUMBITS(1) [],
        TX_FIFO_ALWAYS_ON OFFSET(31) NUMBITS(1) []
    ],
    /// CRCOMPOFST1: drive/ODT compensation offsets.
    pub COMP_OFST_1 [
        DQ_ODT_DOWN OFFSET(0) NUMBITS(3) [],
        DQ_ODT_UP OFFSET(3) NUMBITS(3) [],
        CLK_ODT_DOWN OFFSET(6) NUMBITS(3) [],
        CLK_ODT_UP OFFSET(9) NUMBITS(3) [],
        DQ_DRV_DOWN OFFSET(12) NUMBITS(3) [],
        DQ_DRV_UP OFFSET(15) NUMBITS(3) [],
        CLK_DRV_DOWN OFFSET(18) NUMBITS(3) [],
        CLK_DRV_UP OFFSET(21) NUMBITS(3) [],
        CTL_DRV_DOWN OFFSET(24) NUMBITS(3) [],
        CTL_DRV_UP OFFSET(27) NUMBITS(3) []
    ],
    pub TC_DBP [
        T_RCD OFFSET(0) NUMBITS(4) [],
        T_RP OFFSET(4) NUMBITS(4) [],
        T_AA OFFSET(8) NUMBITS(4) [],
        T_CWL OFFSET(12) NUMBITS(4) [],
        T_RAS OFFSET(16) NUMBITS(8) []
    ],
    pub TC_RAP [
        T_RRD OFFSET(0) NUMBITS(4) [],
        T_RTP OFFSET(4) NUMBITS(4) [],
        T_CKE OFFSET(8) NUMBITS(4) [],
        T_WTR OFFSET(12) NUMBITS(4) [],
        T_FAW OFFSET(16) NUMBITS(8) [],
        T_WR OFFSET(24) NUMBITS(5) [],
        /// Always drive the command bus.
        DIS_3ST OFFSET(29) NUMBITS(1) [],
        T_CMD OFFSET(30) NUMBITS(2) []
    ],
    pub TC_RWP [
        T_RRDR OFFSET(0) NUMBITS(3) [],
        T_RRDD OFFSET(4) NUMBITS(3) [],
        T_WWDR OFFSET(8) NUMBITS(3) [],
        T_WWDD OFFSET(12) NUMBITS(3) [],
        T_RWDRDD OFFSET(16) NUMBITS(3) [],
        T_WRDRDD OFFSET(20) NUMBITS(3) [],
        T_RWSR OFFSET(24) NUMBITS(3) [],
        DEC_WRD OFFSET(27) NUMBITS(1) []
    ],
    pub TC_OTHP [
        T_XPDLL OFFSET(0) NUMBITS(5) [],
        T_XP OFFSET(5) NUMBITS(3) [],
        T_AONPD OFFSET(8) NUMBITS(4) [],
        T_CPDED OFFSET(12) NUMBITS(2) [],
        T_PRPDEN OFFSET(14) NUMBITS(2) [],
        ODT_DELAY_D0 OFFSET(16) NUMBITS(2) [],
        ODT_DELAY_D1 OFFSET(18) NUMBITS(2) []
    ],
    /// Ivy Bridge only.
    pub TC_DTP [
        OVERCLOCK_T_XP OFFSET(12) NUMBITS(1) [],
        OVERCLOCK_T_XPDLL OFFSET(13) NUMBITS(1) []
    ],
    pub TC_RFP [
        OREF_RI OFFSET(0) NUMBITS(8) [],
        REFRESH_HIGH_WM OFFSET(8) NUMBITS(4) [],
        REFRESH_PANIC_WM OFFSET(12) NUMBITS(4) [],
        REFRESH_2X_CONTROL OFFSET(16) NUMBITS(2) []
    ],
    pub TC_RFTP [
        T_REFI OFFSET(0) NUMBITS(16) [],
        T_RFC OFFSET(16) NUMBITS(9) [],
        T_REFIX9 OFFSET(25) NUMBITS(7) []
    ],
    pub TC_SRFTP [
        T_XSDLL OFFSET(0) NUMBITS(12) [],
        T_XS_OFFSET OFFSET(12) NUMBITS(4) [],
        T_ZQOPER OFFSET(16) NUMBITS(10) [],
        T_MOD OFFSET(28) NUMBITS(4) []
    ],
    pub SCHED_CBIT [
        /// Disable the refresh machine during training.
        DIS_REFRESH OFFSET(21) NUMBITS(1) [],
        /// Data scrambling disable.
        DIS_SCRAMBLE OFFSET(28) NUMBITS(1) []
    ],
    /// IOSAV_n_SP_CMD_CTRL.
    pub SP_CMD_CTRL [
        COMMAND OFFSET(0) NUMBITS(16) [],
        RANKSEL_AP OFFSET(16) NUMBITS(2) []
    ],
    /// IOSAV_n_SUBSEQ_CTRL.
    pub SUBSEQ_CTRL [
        CMD_EXECUTIONS OFFSET(0) NUMBITS(9) [],
        CMD_DELAY_GAP OFFSET(10) NUMBITS(5) [],
        POST_SSQ_WAIT OFFSET(16) NUMBITS(9) [],
        DATA_DIRECTION OFFSET(26) NUMBITS(2) [
            None = 0,
            Read = 1,
            Write = 2,
            ReadWrite = 3
        ]
    ],
    /// IOSAV_n_SP_CMD_ADDR.
    pub SP_CMD_ADDR [
        ADDRESS OFFSET(0) NUMBITS(16) [],
        ROWBITS OFFSET(16) NUMBITS(3) [],
        BANK OFFSET(20) NUMBITS(3) [],
        RANK OFFSET(24) NUMBITS(2) []
    ],
    /// IOSAV_n_ADDR_UPDATE.
    pub ADDR_UPDATE [
        INC_ADDR_1 OFFSET(0) NUMBITS(1) [],
        INC_ADDR_8 OFFSET(1) NUMBITS(1) [],
        INC_BANK OFFSET(2) NUMBITS(1) [],
        INC_RANK OFFSET(3) NUMBITS(2) [],
        ADDR_WRAP OFFSET(5) NUMBITS(5) [],
        LFSR_UPD OFFSET(10) NUMBITS(2) [],
        UPD_RATE OFFSET(12) NUMBITS(4) [],
        LFSR_XORS OFFSET(16) NUMBITS(2) []
    ],
    /// IOSAV_SEQ_CTL.
    pub SEQ_CTL [
        LOOPS OFFSET(0) NUMBITS(8) [],
        LAST_SUBSEQ OFFSET(18) NUMBITS(2) [],
        AS_TIMER OFFSET(22) NUMBITS(1) []
    ],
    /// IOSAV_STATUS.
    pub IOSAV_STATUS [
        IDLE OFFSET(0) NUMBITS(1) [],
        BUSY OFFSET(1) NUMBITS(1) [],
        DONE OFFSET(2) NUMBITS(1) [],
        ERROR OFFSET(3) NUMBITS(1) [],
        PANIC OFFSET(4) NUMBITS(1) [],
        RCOMP OFFSET(5) NUMBITS(1) [],
        DRAINED OFFSET(6) NUMBITS(1) []
    ],
    /// MC_INIT_STATE_G.
    pub MC_INIT_STATE_G [
        PURE_SRX OFFSET(0) NUMBITS(1) [],
        DDR_RESET OFFSET(1) NUMBITS(1) [],
        DCLK_ENABLE OFFSET(2) NUMBITS(1) [],
        REFRESH_ENABLE OFFSET(3) NUMBITS(1) [],
        IO_RESET OFFSET(5) NUMBITS(1) [],
        SAFE_SR OFFSET(7) NUMBITS(1) []
    ],
    pub RCOMP_TIMER [
        FIRST_RCOMP_DONE OFFSET(16) NUMBITS(1) []
    ],
    pub MC_BIOS_REQ [
        FREQ OFFSET(0) NUMBITS(8) [],
        REF_100MHZ OFFSET(8) NUMBITS(1) [],
        RUN_BUSY OFFSET(31) NUMBITS(1) []
    ]
];

register_structs! {
    /// One channel's view of a byte lane's training registers.
    pub LaneRegs {
        (0x00 => _reserved0),
        (0x04 => pub training_result: [MmioReadWrite<u32>; 2]),
        (0x0c => _reserved1),
        (0x10 => pub rx: [MmioReadWrite<u32, GDCR_RX::Register>; 4]),
        (0x20 => pub tx: [MmioReadWrite<u32, GDCR_TX::Register>; 4]),
        (0x30 => _reserved2),
        (0x100 => @END),
    },

    /// Per-channel clock I/O registers (`Gz(0x0c00, ch)`).
    pub ClkRegs {
        (0x00 => pub ranks_used: MmioReadWrite<u32>),
        (0x04 => pub comp: MmioReadWrite<u32>),
        (0x08 => _reserved0),
        (0x14 => pub pi_code: MmioReadWrite<u32>),
        (0x18 => pub logic_delay: MmioReadWrite<u32>),
        (0x1c => _reserved1),
        (0x20 => pub dll_fuse: MmioReadWrite<u32>),
        (0x24 => _reserved2),
        (0x3c => pub debug_mux: MmioReadWrite<u32>),
        (0x40 => _reserved3),
        (0x100 => @END),
    },

    /// Per-channel command debug mux (`Gz(0x0e3c, ch)`), holds the write Vref.
    pub CmdMuxRegs {
        (0x00 => _reserved0),
        (0x3c => pub debug_mux: MmioReadWrite<u32>),
        (0x40 => _reserved1),
        (0x100 => @END),
    },

    pub CompRegs {
        (0x00 => _reserved0),
        (0x10 => pub ofst1: MmioReadWrite<u32, COMP_OFST_1::Register>),
        (0x14 => _reserved1),
        (0x100 => @END),
    },

    pub TrainRegs {
        (0x00 => pub training_mod: MmioReadWrite<u32, TRAINING_MOD::Register>),
        (0x04 => pub result: [MmioReadWrite<u32>; 2]),
        (0x0c => _reserved0),
        (0x100 => @END),
    },

    pub CmdRegs {
        (0x00 => pub ctl_ranks_used: MmioReadWrite<u32>),
        (0x04 => pub cmd_comp: MmioReadWrite<u32>),
        (0x08 => pub cmd_ctl_comp: MmioReadWrite<u32>),
        (0x0c => pub pi_coding: MmioReadWrite<u32, CMD_PI_CODING::Register>),
        (0x10 => _reserved0),
        (0x100 => @END),
    },

    /// Memory-controller registers of one channel, or of all channels
    /// through the broadcast bank (`Cx(r, 3)`).
    pub McChannel {
        (0x000 => pub tc_dbp: MmioReadWrite<u32, TC_DBP::Register>),
        (0x004 => pub tc_rap: MmioReadWrite<u32, TC_RAP::Register>),
        (0x008 => pub tc_rwp: MmioReadWrite<u32, TC_RWP::Register>),
        (0x00c => pub tc_othp: MmioReadWrite<u32, TC_OTHP::Register>),
        (0x010 => _reserved0),
        (0x014 => pub tc_dtp: MmioReadWrite<u32, TC_DTP::Register>),
        (0x018 => _reserved1),
        (0x01c => pub sched_second_cbit: MmioReadWrite<u32>),
        (0x020 => pub sched_cbit: MmioReadWrite<u32, SCHED_CBIT::Register>),
        (0x024 => pub roundtrip_latency: MmioReadWrite<u32>),
        (0x028 => pub io_latency: MmioReadWrite<u32>),
        (0x02c => _reserved2),
        (0x034 => pub scrambling_seed_1: MmioReadWrite<u32>),
        (0x038 => pub scrambling_seed_2_lo: MmioReadWrite<u32>),
        (0x03c => pub scrambling_seed_2_hi: MmioReadWrite<u32>),
        (0x040 => pub bw_serror: [MmioReadWrite<u32>; 9]),
        (0x064 => _reserved3),
        (0x080 => pub bw_mask: [MmioReadWrite<u32>; 9]),
        (0x0a4 => _reserved4),
        (0x0a8 => pub pr_cnt_config: MmioReadWrite<u32>),
        (0x0ac => pub pcit: MmioReadWrite<u32>),
        (0x0b0 => pub pm_pdwn_config: MmioReadWrite<u32>),
        (0x0b4 => pub ecc_inject_count: MmioReadWrite<u32>),
        (0x0b8 => pub ecc_dft: MmioReadWrite<u32>),
        (0x0bc => _reserved5),
        (0x0d0 => pub wr_add_delay: MmioReadWrite<u32>),
        /// Ivy Bridge only, broadcast bank only.
        (0x0d4 => pub wmm_read_config: MmioReadWrite<u32>),
        (0x0d8 => _reserved6),
        (0x140 => pub bw_serror_c: [MmioReadWrite<u32>; 9]),
        (0x164 => _reserved7),
        (0x200 => pub sp_cmd_addr: [MmioReadWrite<u32, SP_CMD_ADDR::Register>; 4]),
        (0x210 => pub addr_update: [MmioReadWrite<u32, ADDR_UPDATE::Register>; 4]),
        (0x220 => pub sp_cmd_ctrl: [MmioReadWrite<u32, SP_CMD_CTRL::Register>; 4]),
        (0x230 => pub subseq_ctrl: [MmioReadWrite<u32, SUBSEQ_CTRL::Register>; 4]),
        (0x240 => pub address_lfsr: [MmioReadWrite<u32>; 4]),
        (0x250 => _reserved8),
        (0x280 => pub pm_thml_stat: MmioReadWrite<u32>),
        (0x284 => pub seq_ctl: MmioReadWrite<u32, SEQ_CTL::Register>),
        (0x288 => pub data_ctl: MmioReadWrite<u32>),
        (0x28c => pub status: MmioReadWrite<u32, IOSAV_STATUS::Register>),
        (0x290 => pub tc_zqcal: MmioReadWrite<u32>),
        (0x294 => pub tc_rfp: MmioReadWrite<u32, TC_RFP::Register>),
        (0x298 => pub tc_rftp: MmioReadWrite<u32, TC_RFTP::Register>),
        (0x29c => pub tc_mr2_shadow: MmioReadWrite<u32>),
        (0x2a0 => pub mc_init_state: MmioReadWrite<u32>),
        (0x2a4 => pub tc_srftp: MmioReadWrite<u32, TC_SRFTP::Register>),
        /// Ivy Bridge only, broadcast bank only.
        (0x2a8 => pub mcmnts_spare: MmioReadWrite<u32>),
        (0x2ac => pub iosav_error: MmioReadWrite<u32>),
        (0x2b0 => pub iosav_dc_mask: MmioReadWrite<u32>),
        (0x2b4 => _reserved9),
        (0x340 => pub error_count: [MmioReadWrite<u32>; 9]),
        (0x364 => pub g_error_count: MmioReadWrite<u32>),
        /// Ivy Bridge only.
        (0x368 => pub byte_serror: MmioReadWrite<u32>),
        /// Ivy Bridge only.
        (0x36c => pub byte_serror_c: MmioReadWrite<u32>),
        (0x370 => _reserved10),
        (0x380 => pub pm_trml_m_config: MmioReadWrite<u32>),
        (0x384 => pub pm_cmd_pwr: MmioReadWrite<u32>),
        (0x388 => pub pm_bw_limit_config: MmioReadWrite<u32>),
        (0x38c => pub wdbwm: MmioReadWrite<u32>),
        (0x390 => _reserved11),
        (0x400 => @END),
    },

    /// MCHBAR, 32 KiB. Byte-lane training blocks are interleaved with the
    /// clock/command blocks at the lane bases coreboot calls `LANEBASE_*`.
    pub Mchbar {
        (0x0000 => pub lane_b0: [LaneRegs; 2]),
        (0x0200 => pub lane_b1: [LaneRegs; 2]),
        (0x0400 => pub lane_b2: [LaneRegs; 2]),
        (0x0600 => pub lane_b3: [LaneRegs; 2]),
        (0x0800 => pub lane_ecc: [LaneRegs; 2]),
        (0x0a00 => _reserved0),
        (0x0c00 => pub clk: [ClkRegs; 2]),
        (0x0e00 => pub cmd_mux: [CmdMuxRegs; 2]),
        (0x1000 => pub lane_b4: [LaneRegs; 2]),
        (0x1200 => pub lane_b5: [LaneRegs; 2]),
        (0x1400 => pub lane_b6: [LaneRegs; 2]),
        (0x1600 => pub lane_b7: [LaneRegs; 2]),
        (0x1800 => pub comp: [CompRegs; 2]),
        (0x1a00 => _reserved1),
        (0x3000 => pub train: [TrainRegs; 2]),
        (0x3200 => pub cmd: [CmdRegs; 2]),
        (0x3400 => pub training_mod: MmioReadWrite<u32, TRAINING_MOD::Register>),
        (0x3404 => _reserved2),
        (0x340c => pub data_comp: MmioReadWrite<u32>),
        (0x3410 => _reserved3),
        (0x3714 => pub comp_ofst2: MmioReadWrite<u32>),
        (0x3718 => _reserved4),
        /// Channels 0, 1, (absent 2) and the broadcast bank 3.
        (0x4000 => pub mc: [McChannel; 4]),
        (0x5000 => pub mad_chnl: MmioReadWrite<u32>),
        (0x5004 => pub mad_dimm: [MmioReadWrite<u32>; 3]),
        (0x5010 => _reserved5),
        (0x5014 => pub mad_zr: MmioReadWrite<u32>),
        (0x5018 => pub mcdecs_spare: MmioReadWrite<u32>),
        (0x501c => pub mcdecs_cbit: MmioReadWrite<u32>),
        (0x5020 => _reserved6),
        /// Ivy Bridge only.
        (0x5024 => pub channel_hash: MmioReadWrite<u32>),
        (0x5028 => _reserved7),
        (0x5030 => pub mc_init_state_g: MmioReadWrite<u32, MC_INIT_STATE_G::Register>),
        (0x5034 => pub mrc_revision: MmioReadWrite<u32>),
        (0x5038 => _reserved8),
        (0x5064 => pub pm_dll_config: MmioReadWrite<u32>),
        (0x5068 => _reserved9),
        (0x5084 => pub rcomp_timer: MmioReadWrite<u32, RCOMP_TIMER::Register>),
        (0x5088 => _reserved10),
        (0x5090 => pub ecc_inj_addr_compare: MmioReadWrite<u32>),
        (0x5094 => pub ecc_inj_addr_mask: MmioReadWrite<u32>),
        (0x5098 => _reserved11),
        (0x50fc => pub mc_lock: MmioReadWrite<u8>),
        (0x50fd => _reserved12),
        (0x5400 => pub gfxvtbar: MmioReadWrite<u32>),
        (0x5404 => pub gfxvtbar_hi: MmioReadWrite<u32>),
        (0x5408 => _reserved13),
        (0x5410 => pub vtvc0bar: MmioReadWrite<u32>),
        (0x5414 => pub vtvc0bar_hi: MmioReadWrite<u32>),
        (0x5418 => pub intrdirctl: MmioReadWrite<u32>),
        (0x541c => _reserved14),
        (0x5500 => pub pavp_msg: MmioReadWrite<u32>),
        (0x5504 => _reserved15),
        (0x5880 => pub mem_trml_estimation_config: MmioReadWrite<u32>),
        (0x5884 => _reserved16),
        (0x5888 => pub mem_trml_thresholds_config: MmioReadWrite<u32>),
        (0x588c => _reserved17),
        (0x58a8 => pub mem_trml_interrupt: MmioReadWrite<u32>),
        (0x58ac => _reserved18),
        /// Package power SKU and units, mirrored from the MSRs.
        (0x5930 => pub pkg_power_sku: MmioReadWrite<u32>),
        (0x5934 => _reserved18a),
        (0x5938 => pub pkg_power_sku_unit: MmioReadWrite<u32>),
        (0x593c => _reserved18b),
        /// GT RP state capabilities: RPn in [22:16].
        (0x5998 => pub rp_state_cap: MmioReadWrite<u32>),
        (0x599c => _reserved18c),
        (0x59a0 => pub pkg_power_limit_lo: MmioReadWrite<u32>),
        (0x59a4 => pub pkg_power_limit_hi: MmioReadWrite<u32>),
        (0x59a8 => _reserved19),
        (0x5d10 => pub sskpd: MmioReadWrite<u32>),
        (0x5d14 => pub sskpd_hi: MmioReadWrite<u32>),
        (0x5d18 => _reserved20),
        (0x5da8 => pub bios_reset_cpl: MmioReadWrite<u8>),
        (0x5da9 => _reserved21),
        (0x5e00 => pub mc_bios_req: MmioReadWrite<u32, MC_BIOS_REQ::Register>),
        (0x5e04 => pub mc_bios_data: MmioReadWrite<u32>),
        (0x5e08 => _reserved22),
        (0x5f00 => pub sapmctl: MmioReadWrite<u32>),
        (0x5f04 => _reserved23),
        (0x5f08 => pub m_comp: MmioReadWrite<u32>),
        (0x5f0c => _reserved24),
        (0x5f10 => pub sapmtimers: MmioReadWrite<u32>),
        (0x5f14 => _reserved25),
        /// BANDTIMERS on Sandy Bridge, SAPMTIMERS2 on Ivy Bridge.
        (0x5f18 => pub sapmtimers2_bandtimers_snb: MmioReadWrite<u32>),
        (0x5f1c => _reserved26),
        /// Ivy Bridge only.
        (0x5f20 => pub bandtimers_ivb: MmioReadWrite<u32>),
        (0x5f24 => _reserved27),
        (0x6008 => pub hdaudrid: MmioReadWrite<u32>),
        (0x600c => _reserved28),
        (0x6020 => pub umagfxctl: MmioReadWrite<u32>),
        (0x6024 => _reserved29),
        (0x6030 => pub vdmbdfbarkvm: MmioReadWrite<u32>),
        (0x6034 => pub vdmbdfbarpavp: MmioReadWrite<u32>),
        (0x6038 => _reserved30),
        /// Virtual legacy wire control.
        (0x6120 => pub vlw: MmioReadWrite<u32>),
        (0x6124 => _reserved31),
        (0x63fc => pub vtdtrklck: MmioReadWrite<u32>),
        (0x6400 => _reserved32),
        (0x6800 => pub reqlim: MmioReadWrite<u32>),
        (0x6804 => _reserved33),
        (0x7000 => pub dmivclim: MmioReadWrite<u32>),
        (0x7004 => _reserved34),
        (0x7010 => pub pegctl: MmioReadWrite<u32>),
        (0x7014 => _reserved35),
        (0x740c => pub crdtctl3: MmioReadWrite<u32>),
        (0x7410 => pub crdtctl4: MmioReadWrite<u32>),
        (0x7414 => _reserved36),
        (0x77fc => pub crdtlck: MmioReadWrite<u32>),
        (0x7800 => @END),
    }
}

impl Mchbar {
    /// Training registers of `lane` (0..8, 8 is ECC) on `channel`.
    pub fn lane(&self, channel: usize, lane: usize) -> &LaneRegs {
        let group = match lane {
            0 => &self.lane_b0,
            1 => &self.lane_b1,
            2 => &self.lane_b2,
            3 => &self.lane_b3,
            4 => &self.lane_b4,
            5 => &self.lane_b5,
            6 => &self.lane_b6,
            7 => &self.lane_b7,
            8 => &self.lane_ecc,
            _ => panic!("Sandy Bridge has nine byte lanes"),
        };
        &group[channel]
    }
}

/// The memory controller broadcast bank.
pub const BROADCAST: usize = 3;

register_bitfields![u16,
    pub DMI_LSTS [
        SPEED OFFSET(0) NUMBITS(4) [],
        WIDTH OFFSET(4) NUMBITS(5) [],
        TRAINING OFFSET(11) NUMBITS(1) []
    ],
    pub VC_RSTS [
        NEGOTIATION_PENDING OFFSET(1) NUMBITS(1) []
    ]
];

register_structs! {
    /// DMIBAR (4 KiB).
    pub Dmibar {
        (0x000 => pub vcech: MmioReadWrite<u32>),
        (0x004 => pub pvccap1: MmioReadWrite<u32>),
        (0x008 => pub pvccap2: MmioReadWrite<u32>),
        (0x00c => pub pvcctl: MmioReadWrite<u16>),
        (0x00e => _reserved0),
        (0x010 => pub vc0rcap: MmioReadWrite<u32>),
        (0x014 => pub vc0rctl: MmioReadWrite<u32>),
        (0x018 => _reserved1),
        (0x01a => pub vc0rsts: MmioReadWrite<u16, VC_RSTS::Register>),
        (0x01c => pub vc1rcap: MmioReadWrite<u32>),
        (0x020 => pub vc1rctl: MmioReadWrite<u32>),
        (0x024 => _reserved2),
        (0x026 => pub vc1rsts: MmioReadWrite<u16, VC_RSTS::Register>),
        (0x028 => pub vcprcap: MmioReadWrite<u32>),
        (0x02c => pub vcprctl: MmioReadWrite<u32>),
        (0x030 => _reserved3),
        (0x032 => pub vcprsts: MmioReadWrite<u16, VC_RSTS::Register>),
        (0x034 => pub vcmrcap: MmioReadWrite<u32>),
        (0x038 => pub vcmrctl: MmioReadWrite<u32>),
        (0x03c => _reserved4),
        (0x03e => pub vcmrsts: MmioReadWrite<u16, VC_RSTS::Register>),
        (0x040 => _reserved5),
        (0x084 => pub lcap: MmioReadWrite<u32>),
        (0x088 => pub lctl: MmioReadWrite<u16>),
        (0x08a => pub lsts: MmioReadWrite<u16, DMI_LSTS::Register>),
        (0x08c => _reserved6),
        (0x1c4 => pub uests: MmioReadWrite<u32>),
        (0x1c8 => _reserved7),
        (0x1d0 => pub cests: MmioReadWrite<u32>),
        (0x1d4 => _reserved8),
        (0x1f8 => pub cir_1f8: MmioReadWrite<u32>),
        (0x1fc => pub cir_1fc: MmioReadWrite<u32>),
        (0x200 => _reserved9),
        (0x22c => pub l0slat: MmioReadWrite<u32>),
        (0x230 => _reserved10),
        (0x238 => pub lltc: MmioReadWrite<u32>),
        (0x23c => _reserved11),
        (0x250 => pub cir_250: MmioReadWrite<u32>),
        (0x254 => _reserved12),
        (0x258 => pub cir_258: MmioReadWrite<u32>),
        (0x25c => _reserved13),
        /// Ivy Bridge DMI analog recipe: two lanes groups of 0x20.
        (0x700 => pub afe_700: [DmiAfePair; 2]),
        (0x740 => _reserved14),
        (0x900 => pub afe_900: [DmiAfeBundle; 2]),
        (0x940 => _reserved15),
        (0xa00 => pub afe_a00: [DmiAfeLane; 4]),
        (0xa40 => _reserved16),
        (0xbc8 => pub cir_bc8: MmioReadWrite<u32>),
        (0xbcc => _reserved17),
        (0xc00 => pub afe_c00: MmioReadWrite<u32>),
        (0xc04 => pub afe_c04: MmioReadWrite<u32>),
        (0xc08 => _reserved18),
        (0xc0c => pub afe_c0c: MmioReadWrite<u32>),
        (0xc10 => _reserved19),
        (0xc30 => pub afe_c30: MmioReadWrite<u32>),
        (0xc34 => pub cir_c34: MmioReadWrite<u32>),
        (0xc38 => _reserved20),
        (0xd04 => pub cir_d04: MmioReadWrite<u32>),
        (0xd08 => _reserved21),
        (0x1000 => @END),
    },

    pub DmiAfePair {
        (0x00 => pub reg: MmioReadWrite<u32>),
        (0x04 => _reserved0),
        (0x20 => @END),
    },

    pub DmiAfeBundle {
        (0x00 => pub r900: MmioReadWrite<u32>),
        (0x04 => pub r904: MmioReadWrite<u32>),
        (0x08 => pub r908: MmioReadWrite<u32>),
        (0x0c => pub r90c: MmioReadWrite<u32>),
        (0x10 => pub r910: MmioReadWrite<u32>),
        (0x14 => pub r914: MmioReadWrite<u32>),
        (0x18 => _reserved0),
        (0x20 => @END),
    },

    pub DmiAfeLane {
        (0x00 => pub ra00: MmioReadWrite<u32>),
        (0x04 => pub ra04: MmioReadWrite<u32>),
        (0x08 => _reserved0),
        (0x10 => @END),
    }
}
