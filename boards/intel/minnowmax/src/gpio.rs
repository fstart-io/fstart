//! MinnowBoard MAX GPIO tables (coreboot `mb/intel/minnowmax/gpio.c`),
//! indexed by GPIO number. For multiplexed functions see the EDS ball
//! map; the pad lists are in the BWG volume 2, chapter 37.

use fstart_platform_intel::baytrail::{Pad, Pull, Strength};

/// A legacy-I/O output driven high.
const OUT_HIGH_LEGACY: Pad = Pad::output(true);

#[rustfmt::skip]
pub static NCORE: [Pad; 27] = [
    Pad::native(2), // GPIO_S0_NC[00] - HDMI_HPD
    Pad::native(2), // GPIO_S0_NC[01] - HDMI_DDCDAT
    Pad::native(2), // GPIO_S0_NC[02] - HDMI_DDCCLK
    Pad::nc(), // GPIO_S0_NC[03] - No Connect
    Pad::nc(), // GPIO_S0_NC[04] - No Connect
    Pad::nc(), // GPIO_S0_NC[05] - No Connect
    Pad::nc(), // GPIO_S0_NC[06] - No Connect
    Pad::native(2), // GPIO_S0_NC[07] - DDI1_DDCDAT
    Pad::nc(), // GPIO_S0_NC[08] - No Connect
    Pad::nc(), // GPIO_S0_NC[09] - No Connect
    Pad::nc(), // GPIO_S0_NC[10] - No Connect
    Pad::nc(), // GPIO_S0_NC[11] - No Connect
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S0_NC[12] - TP15
    Pad::nc(), // GPIO_S0_NC[13] - No Connect
    Pad::nc(), // GPIO_S0_NC[14] - No Connect
    Pad::nc(), // GPIO_S0_NC[15] - No Connect
    Pad::nc(), // GPIO_S0_NC[16] - No Connect
    Pad::nc(), // GPIO_S0_NC[17] - No Connect
    Pad::nc(), // GPIO_S0_NC[18] - No Connect
    Pad::nc(), // GPIO_S0_NC[19] - No Connect
    Pad::nc(), // GPIO_S0_NC[20] - No Connect
    Pad::nc(), // GPIO_S0_NC[21] - No Connect
    Pad::nc(), // GPIO_S0_NC[22] - No Connect
    Pad::nc(), // GPIO_S0_NC[23] - No Connect
    Pad::nc(), // GPIO_S0_NC[24] - No Connect
    Pad::nc(), // GPIO_S0_NC[25] - No Connect
    Pad::nc(), // GPIO_S0_NC[26] - No Connect
];

#[rustfmt::skip]
pub static SCORE: [Pad; 102] = [
    Pad::native(1), // GPIO_S0_SC[000] - SATA_GP0
    Pad::native(1), // GPIO_S0_SC[001] - SATA_GP1
    Pad::native(1), // GPIO_S0_SC[002] - SATA_LED_B
    Pad::native(1), // GPIO_S0_SC[003] - PCIE_CLKREQ_0
    Pad::native(1), // GPIO_S0_SC[004] - PCIE_CLKREQ_1
    Pad::native(1), // GPIO_S0_SC[005] - PCIE_CLKREQ_2
    Pad::native(1), // GPIO_S0_SC[006] - PCIE_CLKREQ_3
    Pad::native(2), // GPIO_S0_SC[007] - SD3_WP
    Pad::nc(), // GPIO_S0_SC[008] - No Connect
    Pad::nc(), // GPIO_S0_SC[009] - No Connect
    Pad::nc(), // GPIO_S0_SC[010] - No Connect
    Pad::nc(), // GPIO_S0_SC[011] - No Connect
    Pad::nc(), // GPIO_S0_SC[012] - No Connect
    Pad::nc(), // GPIO_S0_SC[013] - No Connect
    Pad::nc(), // GPIO_S0_SC[014] - No Connect
    Pad::nc(), // GPIO_S0_SC[015] - No Connect
    Pad::nc(), // GPIO_S0_SC[016] - No Connect
    Pad::nc(), // GPIO_S0_SC[017] - No Connect
    Pad::nc(), // GPIO_S0_SC[018] - No Connect
    Pad::nc(), // GPIO_S0_SC[019] - No Connect
    Pad::nc(), // GPIO_S0_SC[020] - No Connect
    Pad::nc(), // GPIO_S0_SC[021] - No Connect
    Pad::nc(), // GPIO_S0_SC[022] - No Connect
    Pad::nc(), // GPIO_S0_SC[023] - No Connect
    Pad::nc(), // GPIO_S0_SC[024] - No Connect
    Pad::nc(), // GPIO_S0_SC[025] - No Connect
    Pad::nc(), // GPIO_S0_SC[026] - No Connect
    Pad::nc(), // GPIO_S0_SC[027] - No Connect
    Pad::nc(), // GPIO_S0_SC[028] - No Connect
    Pad::nc(), // GPIO_S0_SC[029] - No Connect
    Pad::nc(), // GPIO_S0_SC[030] - No Connect
    Pad::nc(), // GPIO_S0_SC[031] - No Connect
    Pad::nc(), // GPIO_S0_SC[032] - No Connect
    Pad::native(1), // GPIO_S0_SC[033] - SD3_CLK
    Pad::native(1), // GPIO_S0_SC[034] - SD3_D0
    Pad::native(1), // GPIO_S0_SC[035] - SD3_D1
    Pad::native(1), // GPIO_S0_SC[036] - SD3_D2
    Pad::native(1), // GPIO_S0_SC[037] - SD3_D3
    Pad::native(1), // GPIO_S0_SC[038] - SD3_CD#
    Pad::native(1), // GPIO_S0_SC[039] - SD3_CMD
    Pad::native(1), // GPIO_S0_SC[040] - TP12 (SD3_1P8EN)
    Pad::native(1), // GPIO_S0_SC[041] - TP11 (/SD3_PWREN)
    Pad::nc(), // GPIO_S0_SC[042] - No Connect
    Pad::nc(), // GPIO_S0_SC[043] - No Connect
    Pad::nc(), // GPIO_S0_SC[044] - No Connect
    Pad::nc(), // GPIO_S0_SC[045] - No Connect
    Pad::nc(), // GPIO_S0_SC[046] - No Connect
    Pad::nc(), // GPIO_S0_SC[047] - No Connect
    Pad::nc(), // GPIO_S0_SC[048] - No Connect
    Pad::nc(), // GPIO_S0_SC[049] - No Connect
    Pad::nc(), // GPIO_S0_SC[050] - No Connect
    Pad::native(1), // GPIO_S0_SC[051] - PCU_SMB_DATA
    Pad::native(1), // GPIO_S0_SC[052] - PCU_SMB_CLK
    Pad::native(1), // GPIO_S0_SC[053] - PCU_SMB_ALERT
    Pad::native(1), // GPIO_S0_SC[054] - ILB_8254_SPKR
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S0_SC[055] - TP8 (GPIO_S0_SC_55)
    Pad::native(0), // GPIO_S0_SC[056] - GPIO_S0_SC_56
    Pad::native(1), // GPIO_S0_SC[057] - PCU_UART3_TXD
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S0_SC[058] - TP9 (GPIO_S0_SC_58)
    Pad::native(0), // GPIO_S0_SC[059] - HDMI_DCDC_ENB
    Pad::native(0), // GPIO_S0_SC[060] - HDMI_LDSW_ENB
    Pad::native(1), // GPIO_S0_SC[061] - PCU_UART3_RXD
    Pad::native(1), // GPIO_S0_SC[062] - LPE_I2S_CLK
    Pad::native(1), // GPIO_S0_SC[063] - LPE_I2S_FRM
    Pad::native(1), // GPIO_S0_SC[064] - LPE_I2S_DATIN
    Pad::native(1), // GPIO_S0_SC[065] - LPE_I2S_DATOUT
    Pad::native(1), // GPIO_S0_SC[066] - SOC_SIO_SPI_CS1
    Pad::native(1), // GPIO_S0_SC[067] - SOC_SIO_SPI_MISO
    Pad::native(1), // GPIO_S0_SC[068] - SOC_SIO_SPI_MOSI
    Pad::native(1), // GPIO_S0_SC[069] - SOC_SIO_SPI_CLK
    Pad::native(1), // GPIO_S0_SC[070] - SIO_UART1_RXD
    Pad::native(1), // GPIO_S0_SC[071] - SIO_UART1_TXD
    Pad::native(1), // GPIO_S0_SC[072] - SIO_UART1_RTSB
    Pad::native(1), // GPIO_S0_SC[073] - SIO_UART1_CTSB
    Pad::native(1), // GPIO_S0_SC[074] - SIO_UART2_RXD
    Pad::native(1), // GPIO_S0_SC[075] - SIO_UART2_TXD
    Pad::nc(), // GPIO_S0_SC[076] - No Connect
    Pad::nc(), // GPIO_S0_SC[077] - No Connect
    Pad::nc(), // GPIO_S0_SC[078] - No Connect
    Pad::nc(), // GPIO_S0_SC[079] - No Connect
    Pad::native(1), // GPIO_S0_SC[080] - TP6 (SIO_I2C1_SDA)
    Pad::native(1), // GPIO_S0_SC[081] - TP5 (SIO_I2C1_SCL)
    Pad::nc(), // GPIO_S0_SC[082] - No Connect
    Pad::nc(), // GPIO_S0_SC[083] - No Connect
    Pad::nc(), // GPIO_S0_SC[084] - No Connect
    Pad::nc(), // GPIO_S0_SC[085] - No Connect
    Pad::nc(), // GPIO_S0_SC[086] - No Connect
    Pad::nc(), // GPIO_S0_SC[087] - No Connect
    Pad::native(1), // GPIO_S0_SC[088] - LSS_I2C_SDA
    Pad::native(1), // GPIO_S0_SC[089] - LSS_I2C_SCL
    Pad::native(1), // GPIO_S0_SC[090] - EXP_I2C_SDA
    Pad::native(1), // GPIO_S0_SC[091] - EXP_I2C_SCL
    Pad::func(1, Pull::Up, Strength::K20), // GPIO_S0_SC[092] - TP13
    Pad::func(1, Pull::Up, Strength::K20), // GPIO_S0_SC[093] - TP16
    Pad::native(1), // GPIO_S0_SC[094] - SOC_PWM0
    Pad::native(1), // GPIO_S0_SC[095] - SOC_PWM1
    Pad::nc(), // GPIO_S0_SC[096] - No Connect
    Pad::nc(), // GPIO_S0_SC[097] - No Connect
    Pad::nc(), // GPIO_S0_SC[098] - No Connect
    Pad::nc(), // GPIO_S0_SC[099] - No Connect
    Pad::nc(), // GPIO_S0_SC[100] - No Connect
    Pad::nc(), // GPIO_S0_SC[101] - No Connect
];

#[rustfmt::skip]
pub static SSUS: [Pad; 44] = [
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[00] - SOC_GPIO_S5_0
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[01] - SOC_GPIO_S5_1
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[02] - SOC_GPIO_S5_2
    Pad::func(6, Pull::Up, Strength::K2), // GPIO_S5[03] - mPCIE_WAKEB
    Pad::input(Pull::None, Strength::K20), // GPIO_S5[04] - BOM_OP0 (Turbot)
    Pad::input(Pull::None, Strength::K20), // GPIO_S5[05] - BOM_OP1
    Pad::input(Pull::None, Strength::K20), // GPIO_S5[06] - BOM_OP2
    Pad::input(Pull::None, Strength::K20), // GPIO_S5[07] - BOM_OP3
    OUT_HIGH_LEGACY, // GPIO_S5[08] - SOC_USB_HOST_EN0
    OUT_HIGH_LEGACY, // GPIO_S5[09] - SOC_USB_HOST_EN1
    OUT_HIGH_LEGACY, // GPIO_S5[10] - GPIO_S5_10_UNLOCK
    Pad::native(0), // GPIO_S5[11] - SUSPWRDNACK (TP14)
    Pad::native(0), // GPIO_S5[12] - PMC_SUSCLK0
    Pad::native(1), // GPIO_S5[13] - PMC_SLP_S0IX (TP10)
    Pad::native(1), // GPIO_S5[14] - GPIO_S514_J20
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[15] - PMC_PCIE_WAKE_R
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[16] - PMC_PWRBTN
    Pad::nc(), // GPIO_S5[17] - No Connect
    Pad::native(1), // GPIO_S5[18] - LPCPD_L (TP7)
    Pad::native(0), // GPIO_S5[19] - SOC_USB_HOST_OC0
    Pad::native(0), // GPIO_S5[20] - SOC_USB_HOST_OC1
    Pad::native(0), // GPIO_S5[21] - SOC_SPI_CS1B
    Pad::input(Pull::Down, Strength::K20), // GPIO_S5[22] - NC or LED D2
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[23] - XDP_H_OBSDATA_A0
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[24] - XDP_H_OBSDATA_A1
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[25] - XDP_H_OBSDATA_A2
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[26] - XDP_H_OBSDATA_A3
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[27] - EXP_GPIO1
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[28] - EXP_GPIO2
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[29] - EXP_GPIO3
    Pad::func(0, Pull::Up, Strength::K20), // GPIO_S5[30] - EXP_GPIO4
    Pad::nc(), // GPIO_S5[31] - No Connect
    Pad::nc(), // GPIO_S5[32] - No Connect
    Pad::nc(), // GPIO_S5[33] - No Connect
    Pad::nc(), // GPIO_S5[34] - No Connect
    Pad::nc(), // GPIO_S5[35] - No Connect
    Pad::nc(), // GPIO_S5[36] - No Connect
    Pad::nc(), // GPIO_S5[37] - No Connect
    Pad::nc(), // GPIO_S5[38] - No Connect
    Pad::nc(), // GPIO_S5[39] - No Connect
    Pad::nc(), // GPIO_S5[40] - No Connect
    Pad::nc(), // GPIO_S5[41] - No Connect
    Pad::nc(), // GPIO_S5[42] - No Connect
    Pad::nc(), // GPIO_S5[43] - No Connect
];
