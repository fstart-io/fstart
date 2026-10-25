//! Board C-state policy consumed by the Core 2 CPU AML.

/// One MWAIT-entered ACPI C-state (`acpi_cstate_t` with an FFixedHW
/// Intel MWAIT register, as in coreboot's mainboard `cstates.c`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CState {
    pub ctype: u8,
    pub latency: u16,
    pub power: u32,
    pub mwait_hint: u8,
}

/// Board `_CST` policy: AC states, and battery states selected through
/// `\PWRS` when non-empty (coreboot `get_bat_cst_entries()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Core2Cstates {
    pub ac: &'static [CState],
    pub battery: &'static [CState],
}

/// Core 2 mobile C1/C2/C3 (coreboot ThinkPad X60/T60 `cstates.c`).
pub const CORE2_MOBILE_CSTATES: Core2Cstates = Core2Cstates {
    ac: &[
        CState {
            ctype: 1,
            latency: 1,
            power: 1000,
            mwait_hint: 0x00,
        },
        CState {
            ctype: 2,
            latency: 1,
            power: 500,
            mwait_hint: 0x10,
        },
        CState {
            ctype: 3,
            latency: 17,
            power: 250,
            mwait_hint: 0x20,
        },
    ],
    battery: &[],
};
