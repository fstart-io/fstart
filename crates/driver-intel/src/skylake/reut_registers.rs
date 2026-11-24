// SPDX-License-Identifier: MIT OR Apache-2.0
//! REUT/LDAT fields from native/reut.h, mc_registers.h and io_test_setup.c.
use tock_registers::register_bitfields;

register_bitfields![u32,
    pub Subsequence [
        COUNT OFFSET(0) NUMBITS(7) [],
        LINEAR OFFSET(7) NUMBITS(1) [],
        WAIT OFFSET(8) NUMBITS(14) [],
        KIND OFFSET(22) NUMBITS(2) [
            Read=0,
            Write=1,
            ReadWrite=2,
            WriteRead=3
        ],
        RESET_BASE OFFSET(27) NUMBITS(1) [],
        ADDRESS_INVERT OFFSET(28) NUMBITS(2) [],
        INVERT OFFSET(30) NUMBITS(1) [],
        STOP_ON_WRAP OFFSET(31) NUMBITS(1) []
    ],
    pub AddressOrder [
        COLUMN OFFSET(0) NUMBITS(2) [],
        ROW OFFSET(2) NUMBITS(2) [],
        BANK OFFSET(4) NUMBITS(2) [],
        RANK OFFSET(6) NUMBITS(2) [],
        WRAP_TRIGGER OFFSET(24) NUMBITS(4) [],
        WRAP_CARRY OFFSET(28) NUMBITS(4) []
    ],
    pub WdbControl [
        RATE OFFSET(0) NUMBITS(6) [],
        LINEAR OFFSET(6) NUMBITS(1) [],
        START OFFSET(8) NUMBITS(6) [],
        END OFFSET(16) NUMBITS(6) []
    ],
    pub WdbMux [
        MUX0 OFFSET(0) NUMBITS(2) [],
        MUX1 OFFSET(3) NUMBITS(2) [],
        MUX2 OFFSET(6) NUMBITS(2) [],
        LFSR OFFSET(8) NUMBITS(3) [],
        ECC_SOURCE OFFSET(15) NUMBITS(1) [],
        SAVE_RATE OFFSET(16) NUMBITS(8) [],
        RELOAD_RATE OFFSET(24) NUMBITS(5) []
    ],
    pub Lmn [
        SWEEP OFFSET(1) NUMBITS(1) [],
        L OFFSET(8) NUMBITS(8) [],
        M OFFSET(16) NUMBITS(8) [],
        N OFFSET(24) NUMBITS(8) []
    ],
    pub CadbControl [ DESELECT OFFSET(0) NUMBITS(1) [] ],
    pub CadbMux [
        MUX0 OFFSET(0) NUMBITS(2) [],
        MUX1 OFFSET(4) NUMBITS(2) [],
        MUX2 OFFSET(8) NUMBITS(2) [],
        LFSR OFFSET(28) NUMBITS(3) []
    ],
    pub LdatSdat [
        BANK OFFSET(0) NUMBITS(3) [],
        REPLICATE OFFSET(3) NUMBITS(1) [],
        CAPTURE_ARRAY OFFSET(5) NUMBITS(1) [],
        MODE OFFSET(16) NUMBITS(1) []
    ],
    pub LdatPdat [
        ADDRESS OFFSET(0) NUMBITS(6) [],
        COMMAND OFFSET(24) NUMBITS(4) [Write=8]
    ],
    pub ErrorControl [
        NTH OFFSET(0) NUMBITS(6) [],
        STOP OFFSET(12) NUMBITS(2) [],
        CHUNKS OFFSET(16) NUMBITS(8) [],
        CACHELINES OFFSET(24) NUMBITS(8) []
    ],
    pub ErrorCounterControl [
        POINTER OFFSET(0) NUMBITS(7) [],
        KIND OFFSET(7) NUMBITS(2) [
            Channel=0,
            Byte=2
        ]
    ],
    pub ErrorCounterStatus [ COUNT OFFSET(0) NUMBITS(23) [] ],
    pub TrainingFeedback [ VALUE OFFSET(0) NUMBITS(9) [] ]
];
register_bitfields![u64,
    pub TestAddress [
        COLUMN OFFSET(3) NUMBITS(9) [],
        ROW OFFSET(24) NUMBITS(17) [],
        BANK OFFSET(48) NUMBITS(4) [],
        RANK OFFSET(56) NUMBITS(3) []
    ],
    pub AddressIncrement [
        COLUMN OFFSET(3) NUMBITS(8) [],
        COLUMN_RATE OFFSET(12) NUMBITS(5) [],
        COLUMN_LINEAR OFFSET(19) NUMBITS(1) [],
        ROW OFFSET(20) NUMBITS(12) [],
        ROW_RATE OFFSET(32) NUMBITS(4) [],
        ROW_LINEAR OFFSET(37) NUMBITS(1) [],
        BANK OFFSET(38) NUMBITS(4) [],
        BANK_RATE OFFSET(44) NUMBITS(5) [],
        BANK_LINEAR OFFSET(51) NUMBITS(1) [],
        RANK OFFSET(52) NUMBITS(3) [],
        RANK_RATE OFFSET(56) NUMBITS(5) [],
        RANK_LINEAR OFFSET(63) NUMBITS(1) []
    ],
    pub ErrorDetail [
        ECC OFFSET(0) NUMBITS(8) [],
        BYTE_GROUPS OFFSET(32) NUMBITS(8) []
    ]
];
