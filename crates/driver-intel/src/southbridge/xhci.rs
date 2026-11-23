// SPDX-License-Identifier: MIT OR Apache-2.0
//! Standard xHCI PORTSC semantics shared by Intel host initialization.
use tock_registers::{LocalRegisterCopy, fields::FieldValue, register_bitfields};
register_bitfields![u32,
    pub PORTSC [
        PED OFFSET(1) NUMBITS(1) [],
        PR OFFSET(4) NUMBITS(1) [],
        PLS OFFSET(5) NUMBITS(4) [],
        PP OFFSET(9) NUMBITS(1) [],
        LWS OFFSET(16) NUMBITS(1) [],
        CSC OFFSET(17) NUMBITS(1) [],
        PEC OFFSET(18) NUMBITS(1) [],
        WRC OFFSET(19) NUMBITS(1) [],
        OCC OFFSET(20) NUMBITS(1) [],
        PRC OFFSET(21) NUMBITS(1) [],
        PLC OFFSET(22) NUMBITS(1) [],
        CEC OFFSET(23) NUMBITS(1) [],
        WPR OFFSET(31) NUMBITS(1) []
    ]
];
pub const CHANGE_MASK: u32 = 0x00fe_0000;
/// A control update must never implicitly acknowledge pending changes, disable
/// a currently enabled port (PED is RW1CS), or restart a reset (PR/WPR).
/// Explicit requested fields can still perform those operations intentionally.
pub(crate) fn portsc_value(snapshot: u32, fields: FieldValue<u32, PORTSC::Register>) -> u32 {
    let mut value = LocalRegisterCopy::<u32, PORTSC::Register>::new(
        snapshot
            & !(CHANGE_MASK
                | PORTSC::PED::SET.value
                | PORTSC::PR::SET.value
                | PORTSC::WPR::SET.value),
    );
    value.modify(fields);
    value.get()
}
