// SPDX-License-Identifier: MIT OR Apache-2.0
//! Dalboz family wiring.
use fstart_driver_amd::{
    gpio::{Delivery, Pad, Pull, Trigger},
    picasso::pinmux::*,
};

pub const GPIO_SET_STAGE_RAM: &[Pad] = &[
    Pad::native(0, GPIO_0_PWR_BTN_L, Pull::None),
    Pad::native(1, GPIO_1_SYS_RESET_L, Pull::None),
    Pad::native_sci(2, GPIO_2_WAKE_L, Pull::None, Trigger::EdgeLow),
    Pad::interrupt(3, Pull::None, Trigger::EdgeLow, Delivery::StatusAndDelivery),
    Pad::wake_s3(4, Pull::None, Trigger::EdgeHigh),
    Pad::output(5, true),
    Pad::output(6, true),
    Pad::native(7, GPIO_7_ACP_I2S_SDIN, Pull::None),
    Pad::native(8, GPIO_8_ACP_I2S_LRCLK, Pull::None),
    Pad::sci(9, Pull::None, Trigger::LevelLow),
    Pad::output(10, true),
    Pad::input(11, Pull::None),
    Pad::input(12, Pull::None),
    Pad::native(16, GPIO_16_USB_OC0_L, Pull::None),
    Pad::native(17, GPIO_17_USB_OC1_L, Pull::None),
    Pad::output(18, false),
    Pad::native(19, GPIO_19_I2C3_SCL, Pull::None),
    Pad::native(20, GPIO_20_I2C3_SDA, Pull::None),
    Pad::native(21, GPIO_21_EMMC_CMD, Pull::None),
    Pad::sci(22, Pull::None, Trigger::EdgeLow),
    Pad::native(23, GPIO_23_AC_PRES, Pull::Up),
    Pad::sci(24, Pull::None, Trigger::EdgeLow),
    Pad::native(26, GPIO_26_PCIE_RST_L, Pull::None),
    Pad::native(30, GPIO_30_ESPI_CS_L, Pull::None),
    Pad::input(31, Pull::None),
    Pad::output(32, true),
    Pad::output(40, true),
    Pad::output(67, false),
    Pad::output(68, true),
    Pad::input(69, Pull::None),
    Pad::native(70, GPIO_70_EMMC_CLK, Pull::None),
    Pad::native(74, GPIO_74_EMMC_DATA4, Pull::None),
    Pad::native(75, GPIO_75_EMMC_DATA6, Pull::None),
    Pad::output(76, true),
    Pad::input(84, Pull::None),
    Pad::output(85, false),
    Pad::output(86, true),
    Pad::native(87, GPIO_87_EMMC_DATA7, Pull::None),
    Pad::native(88, GPIO_88_EMMC_DATA5, Pull::None),
    Pad::nc(89),
    Pad::input(90, Pull::None),
    Pad::output(91, false),
    Pad::native(92, GPIO_92_CLK_REQ0_L, Pull::None),
    Pad::native(104, GPIO_104_EMMC_DATA0, Pull::None),
    Pad::native(105, GPIO_105_EMMC_DATA1, Pull::None),
    Pad::native(106, GPIO_106_EMMC_DATA2, Pull::None),
    Pad::native(107, GPIO_107_EMMC_DATA3, Pull::None),
    Pad::native(108, GPIO_108_ESPI_ALERT_L, Pull::Up),
    Pad::native(109, GPIO_109_EMMC_DS, Pull::None),
    Pad::native(113, GPIO_113_I2C2_SCL, Pull::None),
    Pad::native(114, GPIO_114_I2C2_SDA, Pull::None),
    Pad::native(115, GPIO_115_CLK_REQ1_L, Pull::None),
    Pad::native(116, GPIO_116_CLK_REQ2_L, Pull::None),
    Pad::native(129, GPIO_129_KBRST_L, Pull::None),
    Pad::input(132, Pull::None),
    Pad::input(135, Pull::None),
    Pad::native(136, GPIO_136_UART0_RXD, Pull::None),
    Pad::input(137, Pull::None),
    Pad::native(138, GPIO_138_UART0_TXD, Pull::None),
    Pad::input(139, Pull::None),
    Pad::output(140, true),
    Pad::output(141, true),
    Pad::output(142, true),
    Pad::output(143, false),
    Pad::output(144, false),
];

pub const ESPI_GPIO_TABLE: &[Pad] = &[
    Pad::native(26, GPIO_26_PCIE_RST_L, Pull::None),
    Pad::native(30, GPIO_30_ESPI_CS_L, Pull::None),
    Pad::native(108, GPIO_108_ESPI_ALERT_L, Pull::None),
];

pub const TPM_GPIO_TABLE: &[Pad] = &[
    Pad::interrupt(3, Pull::None, Trigger::EdgeLow, Delivery::Status),
    Pad::native(19, GPIO_19_I2C3_SCL, Pull::None),
    Pad::native(20, GPIO_20_I2C3_SDA, Pull::None),
    Pad::input(11, Pull::None),
];

pub const EARLY_GPIO_TABLE: &[Pad] = &[
    Pad::native(136, GPIO_136_UART0_RXD, Pull::None),
    Pad::native(138, GPIO_138_UART0_TXD, Pull::None),
];

pub const ROMSTAGE_GPIO_TABLE: &[Pad] = &[Pad::output(32, true), Pad::output(140, false)];

/// Source table only. A native flow must account for Vilboz's GPIO40 proximity
/// sensor override before reusing this Dalboz NVMe reset wiring.
pub const PCIE_GPIO_TABLE: &[Pad] = &[
    Pad::native(27, GPIO_27_PCIE_RST1_L, Pull::None),
    Pad::output(40, true),
    Pad::native(92, GPIO_92_CLK_REQ0_L, Pull::None),
    Pad::native(115, GPIO_115_CLK_REQ1_L, Pull::None),
    Pad::native(116, GPIO_116_CLK_REQ2_L, Pull::None),
    Pad::output(142, true),
];
