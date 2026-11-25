// SPDX-License-Identifier: MIT OR Apache-2.0
//! Peppy pad mux, indexed by physical GPIO (variant gpio.c).
use fstart_platform_intel::haswell::{Mode::*, Pad, PadTable};

const INVERTED: Pad = Pad {
    invert: true,
    ..Pad::new(Input)
};

pub static PADS: PadTable = [
    Pad::new(Unused),       // 0: UNUSED
    Pad::new(Unused),       // 1: UNUSED
    Pad::new(Unused),       // 2: UNUSED
    Pad::new(Unused),       // 3: UNUSED
    Pad::new(Native),       // 4: NATIVE: I2C0_SDA_GPIO4
    Pad::new(Native),       // 5: NATIVE: I2C0_SCL_GPIO5
    Pad::new(Native),       // 6: NATIVE: I2C1_SDA_GPIO6
    Pad::new(Native),       // 7: NATIVE: I2C1_SCL_GPIO7
    Pad::new(Sci),          // 8: LTE_WAKE_L_Q
    Pad::new(Input),        // 9: RAM_ID1
    Pad::new(Sci),          // 10: WLAN_WAKE_L_Q
    Pad::new(Unused),       // 11: UNUSED
    INVERTED,               // 12: TRACKPAD_INT_L (WAKE)
    Pad::new(Input),        // 13: RAM_ID0
    Pad::new(Input),        // 14: EC_IN_RW
    Pad::new(Unused),       // 15: UNUSED (STRAP)
    Pad::new(Unused),       // 16: UNUSED
    Pad::new(Unused),       // 17: UNUSED
    Pad::new(Native),       // 18: PCIE_CLKREQ_WLAN#
    Pad::new(Native),       // 19: PCIE_CLKREQ_LTE#
    Pad::new(Unused),       // 20: UNUSED
    Pad::new(Unused),       // 21: UNUSED
    Pad::new(Unused),       // 22: UNUSED
    Pad::new(Unused),       // 23: UNUSED
    Pad::new(Unused),       // 24: UNUSED
    INVERTED,               // 25: TOUCH_INT_L (WAKE)
    Pad::new(Unused),       // 26: UNUSED
    Pad::new(Unused),       // 27: UNUSED
    Pad::new(Unused),       // 28: UNUSED
    Pad::new(Unused),       // 29: UNUSED
    Pad::new(Native),       // 30: NATIVE: PCH_SUSWARN_L
    Pad::new(Native),       // 31: NATIVE: ACPRESENT
    Pad::new(Native),       // 32: NATIVE: LPC_CLKRUN_L
    Pad::new(Native),       // 33: NATIVE: DEVSLP0
    Pad::new(Smi),          // 34: EC_SMI_L
    Pad::new(Smi),          // 35: PCH_NMI_DBG_L (route in NMI_EN)
    Pad::new(Sci),          // 36: EC_SCI_L
    Pad::new(Unused),       // 37: UNUSED
    Pad::new(Unused),       // 38: UNUSED
    Pad::new(Unused),       // 39: UNUSED
    Pad::new(Native),       // 40: NATIVE: USB_OC0#
    Pad::new(Unused),       // 41: UNUSED
    Pad::new(Native),       // 42: NATIVE: USB_OC2#
    Pad::new(Unused),       // 43: UNUSED
    Pad::new(Output(true)), // 44: PP3300_SSD_EN
    Pad::new(Output(true)), // 45: PP3300_CODEC_EN
    Pad::new(Output(true)), // 46: WLAN_DISABLE_L
    Pad::new(Input),        // 47: RAM_ID2
    Pad::new(Unused),       // 48: UNUSED
    Pad::new(Unused),       // 49: UNUSED
    Pad::new(Unused),       // 50: UNUSED
    Pad::new(Pirq),         // 51: ALS_INT_L -> PIRQT/IRQ35 (not plain input as in gpio.c)
    Pad::new(Input),        // 52: SIM_DET
    Pad::new(Pirq),         // 53: TRACKPAD_INT_DX
    Pad::new(Pirq),         // 54: TOUCH_INT_L_DX
    Pad::new(Unused),       // 55: UNUSED
    Pad::new(Unused),       // 56: UNUSED
    Pad::new(Output(true)), // 57: PP3300_CCD_EN
    Pad::new(Input),        // 58: PCH_SPI_WP_D
    Pad::new(Output(true)), // 59: LTE_DISABLE_L
    Pad::new(Native),       // 60: NATIVE: SML0ALERT
    Pad::new(Unused),       // 61: UNUSED
    Pad::new(Unused),       // 62: UNUSED
    Pad::new(Native),       // 63: NATIVE: PCH_SLP_S5_L
    Pad::new(Unused),       // 64: UNUSED
    Pad::new(Unused),       // 65: UNUSED
    Pad::new(Unused),       // 66: UNUSED (STRAP)
    Pad::new(Unused),       // 67: UNUSED
    Pad::new(Unused),       // 68: UNUSED
    Pad::new(Unused),       // 69: UNUSED
    Pad::new(Unused),       // 70: UNUSED
    Pad::new(Native),       // 71: NATIVE: MODPHY_EN
    Pad::new(Native),       // 72: NATIVE: PCH_BATLOW#
    Pad::new(Native),       // 73: NATIVE: SMB1ALERT#
    Pad::new(Native),       // 74: NATIVE: SMB_ME1_DAT
    Pad::new(Native),       // 75: NATIVE: SMB_ME1_CLK
    Pad::new(Unused),       // 76: UNUSED
    Pad::new(Unused),       // 77: UNUSED
    Pad::new(Unused),       // 78: UNUSED
    Pad::new(Unused),       // 79: UNUSED
    Pad::new(Unused),       // 80: UNUSED
    Pad::new(Native),       // 81: NATIVE: SPKR
    Pad::new(Native),       // 82: NATIVE: EC_RCIN_L
    Pad::new(Unused),       // 83: UNUSED
    Pad::new(Unused),       // 84: UNUSED
    Pad::new(Unused),       // 85: UNUSED
    Pad::new(Unused),       // 86: UNUSED (STRAP)
    Pad::new(Unused),       // 87: UNUSED
    Pad::new(Unused),       // 88: UNUSED
    Pad::new(Unused),       // 89: UNUSED
    Pad::new(Unused),       // 90: UNUSED
    Pad::new(Unused),       // 91: UNUSED
    Pad::new(Unused),       // 92: UNUSED
    Pad::new(Unused),       // 93: UNUSED
    Pad::new(Unused),       // 94: UNUSED
];
