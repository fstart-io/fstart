// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 Sunrise Point-LP GPIO pad policy from
//! `mainboard/lenovo/sklkbl_thinkpad/variants/t480/gpio.c`.

use fstart_driver_intel::sunrise_point::{
    dmi::Kind,
    gpio::{Group, Pad, PadConfig},
    gpio_config::{self as cfg, NativeFunction, Owner, Pull, Reset, Route, Trigger},
};

const fn pad(group: Group, pin: u8) -> Pad {
    match group.pad_for(Kind::Lp, pin) {
        Ok(pad) => pad,
        Err(_) => panic!("invalid Sunrise Point-LP pad"),
    }
}

/// dGPU strap and power-control GPIOs from `variants/t480/include/variant/dgpu.h`.
pub const GPIO_DISCRETE_PRESENCE: Pad = pad(Group::D, 9);
pub const GPIO_DGFX_VRAM_ID0: Pad = pad(Group::D, 11);
pub const GPIO_DGFX_VRAM_ID1: Pad = pad(Group::D, 12);
pub const GPIO_GPU_RST: Pad = pad(Group::E, 22);
pub const GPIO_1R8VIDEO_AON_ON: Pad = pad(Group::E, 23);
pub const GPIO_DGFX_PWRGD: Pad = pad(Group::F, 3);

/// GPE0 DW0..DW2 group mapping: puts `GPP_C22` (`-EC_SCI`) at GPE 22 (`0x16`)
/// and `GPP_C23` (`-EC_WAKE`) at GPE 23 (`0x17`), matching `THINKPAD_EC_GPE = 22`.
pub const GPE_GROUPS: [Group; 3] = [Group::C, Group::D, Group::E];

pub const PAD_TABLE: &[PadConfig] = &[
    // ------- GPIO Community 0 -------
    // ------- GPIO Group GPP_A -------
    cfg::native(
        pad(Group::A, 0),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -KBRC
    cfg::native(
        pad(Group::A, 1),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // LPC_AD0
    cfg::native(
        pad(Group::A, 2),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // LPC_AD1
    cfg::native(
        pad(Group::A, 3),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // LPC_AD2
    cfg::native(
        pad(Group::A, 4),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // LPC_AD3
    cfg::native(
        pad(Group::A, 5),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -LPC_FRAME
    cfg::native(
        pad(Group::A, 6),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // IRQSER
    cfg::native(
        pad(Group::A, 7),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -TPM_IRQ
    cfg::native(
        pad(Group::A, 8),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -CLKRUN
    cfg::native(
        pad(Group::A, 9),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // LPCCLK_EC_24M
    cfg::native(
        pad(Group::A, 10),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // LPCCLK_DEBUG_24M
    cfg::not_connected(pad(Group::A, 11), Pull::None),
    cfg::not_connected(pad(Group::A, 12), Pull::None),
    cfg::native(
        pad(Group::A, 13),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -SUSWARN
    cfg::native(
        pad(Group::A, 14),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -SUS_STAT
    cfg::native(
        pad(Group::A, 15),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -SUSWARN
    cfg::not_connected(pad(Group::A, 16), Pull::None),
    cfg::not_connected(pad(Group::A, 17), Pull::None),
    cfg::not_connected(pad(Group::A, 18), Pull::None),
    cfg::not_connected(pad(Group::A, 19), Pull::None),
    cfg::not_connected(pad(Group::A, 20), Pull::None),
    cfg::not_connected(pad(Group::A, 21), Pull::None),
    cfg::not_connected(pad(Group::A, 22), Pull::None),
    cfg::not_connected(pad(Group::A, 23), Pull::None),
    // ------- GPIO Group GPP_B -------
    cfg::not_connected(pad(Group::B, 0), Pull::None),
    cfg::not_connected(pad(Group::B, 1), Pull::None),
    cfg::not_connected(pad(Group::B, 2), Pull::None),
    cfg::not_connected(pad(Group::B, 3), Pull::None),
    cfg::interrupt(
        pad(Group::B, 4),
        Pull::None,
        Reset::Deep,
        Trigger::Edge,
        true,
        Route::Sci,
    ), // -TBT_PLUG_EVENT
    cfg::native(
        pad(Group::B, 5),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -CLKREQ_PCIE0
    cfg::native(
        pad(Group::B, 6),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -CLKREQ_PCIE4
    cfg::native(
        pad(Group::B, 7),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -CLKREQ_PCIE5
    cfg::native(
        pad(Group::B, 8),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -CLKREQ_PCIE6
    cfg::native(
        pad(Group::B, 9),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -CLKREQ_PCIE8
    cfg::native(
        pad(Group::B, 10),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -CLKREQ_PCIE10
    cfg::not_connected(pad(Group::B, 11), Pull::None),
    cfg::native(
        pad(Group::B, 12),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -PCH_SLP_S0
    cfg::native(
        pad(Group::B, 13),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -PLTRST
    cfg::native(
        pad(Group::B, 14),
        Pull::Native,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // PCH_SPKR
    cfg::output(pad(Group::B, 15), true, Reset::Deep), // NFC_DLREQ
    cfg::not_connected(pad(Group::B, 16), Pull::None),
    cfg::not_connected(pad(Group::B, 17), Pull::None),
    cfg::not_connected(pad(Group::B, 18), Pull::None),
    cfg::not_connected(pad(Group::B, 19), Pull::None),
    cfg::not_connected(pad(Group::B, 20), Pull::None),
    cfg::not_connected(pad(Group::B, 21), Pull::None),
    cfg::not_connected(pad(Group::B, 22), Pull::None),
    cfg::not_connected(pad(Group::B, 23), Pull::None),
    // ------- GPIO Community 1 -------
    // ------- GPIO Group GPP_C -------
    cfg::native(
        pad(Group::C, 0),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // SMB_CLK
    cfg::native(
        pad(Group::C, 1),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // SMB_DATA
    cfg::not_connected(pad(Group::C, 2), Pull::None),
    cfg::native(
        pad(Group::C, 3),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // SML0_CLK
    cfg::native(
        pad(Group::C, 4),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // SML0_DATA
    cfg::not_connected(pad(Group::C, 5), Pull::None),
    cfg::native(
        pad(Group::C, 6),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // EC_SCL2
    cfg::native(
        pad(Group::C, 7),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // EC_SDA2
    cfg::not_connected(pad(Group::C, 8), Pull::None),
    cfg::not_connected(pad(Group::C, 9), Pull::None),
    cfg::not_connected(pad(Group::C, 10), Pull::None),
    cfg::not_connected(pad(Group::C, 11), Pull::None),
    cfg::not_connected(pad(Group::C, 12), Pull::None),
    cfg::not_connected(pad(Group::C, 13), Pull::None),
    cfg::not_connected(pad(Group::C, 14), Pull::None),
    cfg::not_connected(pad(Group::C, 15), Pull::None),
    cfg::native(
        pad(Group::C, 16),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // I2C0_DATA
    cfg::native(
        pad(Group::C, 17),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // I2C0_CLK
    cfg::not_connected(pad(Group::C, 18), Pull::None),
    cfg::not_connected(pad(Group::C, 19), Pull::None),
    cfg::output(pad(Group::C, 20), false, Reset::Deep), // EPRIVACY_ON
    cfg::output(pad(Group::C, 21), true, Reset::Platform), // TBT_FORCE_PWR
    cfg::interrupt(
        pad(Group::C, 22),
        Pull::None,
        Reset::Deep,
        Trigger::Edge,
        true,
        Route::Sci,
    ), // -EC_SCI
    cfg::interrupt(
        pad(Group::C, 23),
        Pull::None,
        Reset::Deep,
        Trigger::Edge,
        true,
        Route::Sci,
    ), // -EC_WAKE
    // ------- GPIO Group GPP_D -------
    cfg::not_connected(pad(Group::D, 0), Pull::None),
    cfg::not_connected(pad(Group::D, 1), Pull::None),
    cfg::not_connected(pad(Group::D, 2), Pull::None),
    cfg::not_connected(pad(Group::D, 3), Pull::None),
    cfg::not_connected(pad(Group::D, 4), Pull::None),
    cfg::not_connected(pad(Group::D, 5), Pull::None),
    cfg::not_connected(pad(Group::D, 6), Pull::None),
    cfg::not_connected(pad(Group::D, 7), Pull::None),
    cfg::not_connected(pad(Group::D, 8), Pull::None),
    cfg::input(
        pad(Group::D, 9),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // -DISCRETE_PRESENCE
    cfg::not_connected(pad(Group::D, 10), Pull::None),
    cfg::input(
        pad(Group::D, 11),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // DGFX_VRAM_ID0
    cfg::input(
        pad(Group::D, 12),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // DGFX_VRAM_ID1
    cfg::not_connected(pad(Group::D, 13), Pull::None),
    cfg::not_connected(pad(Group::D, 14), Pull::None),
    cfg::not_connected(pad(Group::D, 15), Pull::None),
    cfg::not_connected(pad(Group::D, 16), Pull::None),
    cfg::output(pad(Group::D, 17), false, Reset::Deep), // DDI_PRIORITY1
    cfg::not_connected(pad(Group::D, 18), Pull::None),
    cfg::not_connected(pad(Group::D, 19), Pull::None),
    cfg::not_connected(pad(Group::D, 20), Pull::None),
    cfg::not_connected(pad(Group::D, 21), Pull::None),
    cfg::input(
        pad(Group::D, 22),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // -NFC_DTCT
    cfg::not_connected(pad(Group::D, 23), Pull::None),
    // ------- GPIO Group GPP_E -------
    cfg::not_connected(pad(Group::E, 0), Pull::None),
    cfg::native(
        pad(Group::E, 1),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -WWAN_SATA_DTCT
    cfg::native(
        pad(Group::E, 2),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -PE_DTCT
    cfg::input(
        pad(Group::E, 3),
        Pull::None,
        Reset::Deep,
        Trigger::Edge,
        Owner::Acpi,
    ), // -TBT_PLUG_EVENT
    cfg::output(pad(Group::E, 4), true, Reset::Deep), // NFC_ON
    cfg::not_connected(pad(Group::E, 5), Pull::None),
    cfg::native(
        pad(Group::E, 6),
        Pull::None,
        Reset::Resume,
        NativeFunction::Nf1,
    ), // SATA2_DEVSLP
    cfg::not_connected(pad(Group::E, 7), Pull::None),
    cfg::not_connected(pad(Group::E, 8), Pull::None),
    cfg::native(
        pad(Group::E, 9),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -USB_PORT0_OC0 (AON port)
    cfg::native(
        pad(Group::E, 10),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // -USB_PORT1_OC1 (regular port)
    cfg::not_connected(pad(Group::E, 11), Pull::None),
    cfg::interrupt(
        pad(Group::E, 12),
        Pull::None,
        Reset::Deep,
        Trigger::Level,
        false,
        Route::IoApic,
    ), // NFC_INT
    cfg::native(
        pad(Group::E, 13),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // DDIP1_HPD
    cfg::native(
        pad(Group::E, 14),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // DDIP2_HPD
    cfg::not_connected(pad(Group::E, 15), Pull::None),
    cfg::not_connected(pad(Group::E, 16), Pull::None),
    cfg::native(
        pad(Group::E, 17),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // EDP_HPD
    cfg::not_connected(pad(Group::E, 18), Pull::None),
    cfg::not_connected(pad(Group::E, 19), Pull::None),
    cfg::native(
        pad(Group::E, 20),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // DDIP2_CTRLCLK
    cfg::native(
        pad(Group::E, 21),
        Pull::None,
        Reset::Deep,
        NativeFunction::Nf1,
    ), // DDIP2_CTRLDATA
    cfg::output_with_pull(pad(Group::E, 22), false, Pull::Up20k, Reset::Resume), // -GPU_RST
    cfg::output_with_pull(pad(Group::E, 23), false, Pull::Up20k, Reset::Resume), // 1R8VIDEO_AON_ON
    // ------- GPIO Community 2 -------
    // -------- GPIO Group GPD --------
    cfg::native(
        pad(Group::Gpd, 0),
        Pull::None,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // -BATLOW
    cfg::native(
        pad(Group::Gpd, 1),
        Pull::Native,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // AC_PRESENT
    cfg::native(
        pad(Group::Gpd, 2),
        Pull::Native,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // -LANWAKE
    cfg::native(
        pad(Group::Gpd, 3),
        Pull::Up20k,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // -PWRSW_EC
    cfg::native(
        pad(Group::Gpd, 4),
        Pull::None,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // -PCH_SLP_S3
    cfg::native(
        pad(Group::Gpd, 5),
        Pull::None,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // -PCH_SLP_S4
    cfg::native(
        pad(Group::Gpd, 6),
        Pull::None,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // -PCH_SLP_M
    cfg::not_connected(pad(Group::Gpd, 7), Pull::None),
    cfg::native(
        pad(Group::Gpd, 8),
        Pull::None,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // SUSCLK_32K
    cfg::native(
        pad(Group::Gpd, 9),
        Pull::None,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // -PCH_SLP_WLAN
    cfg::native(
        pad(Group::Gpd, 10),
        Pull::None,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // -PCH_SLP_S5
    cfg::native(
        pad(Group::Gpd, 11),
        Pull::None,
        Reset::PowerOk,
        NativeFunction::Nf1,
    ), // LANPHYPC
    // ------- GPIO Community 3 -------
    // ------- GPIO Group GPP_F -------
    cfg::not_connected(pad(Group::F, 0), Pull::None),
    cfg::input(
        pad(Group::F, 1),
        Pull::None,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // GC6_FB_EN
    cfg::output(pad(Group::F, 2), true, Reset::Deep), // -GPU_EVENT
    cfg::input(
        pad(Group::F, 3),
        Pull::None,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // DGFX_PWRGD
    cfg::output(pad(Group::F, 4), true, Reset::Deep), // -WWAN_RESET
    cfg::not_connected(pad(Group::F, 5), Pull::None),
    cfg::input(
        pad(Group::F, 6),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // -MIC_HW_EN
    cfg::input(
        pad(Group::F, 7),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // -INT_MIC_DTCT
    cfg::input(
        pad(Group::F, 8),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // WWAN_CFG0
    cfg::input(
        pad(Group::F, 9),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // WWAN_CFG1
    cfg::input(
        pad(Group::F, 10),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // WWAN_CFG2
    cfg::input(
        pad(Group::F, 11),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // WWAN_CFG3
    cfg::input(
        pad(Group::F, 12),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // PLANARID0
    cfg::input(
        pad(Group::F, 13),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // PLANARID1
    cfg::input(
        pad(Group::F, 14),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // PLANARID2
    cfg::input(
        pad(Group::F, 15),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // PLANARID3
    cfg::not_connected(pad(Group::F, 16), Pull::None),
    cfg::not_connected(pad(Group::F, 17), Pull::None),
    cfg::not_connected(pad(Group::F, 18), Pull::None),
    cfg::not_connected(pad(Group::F, 19), Pull::None),
    cfg::not_connected(pad(Group::F, 20), Pull::None),
    cfg::not_connected(pad(Group::F, 21), Pull::None),
    cfg::input(
        pad(Group::F, 22),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // -INTRUDER_PCH
    cfg::input(
        pad(Group::F, 23),
        Pull::Up20k,
        Reset::Deep,
        Trigger::Off,
        Owner::Acpi,
    ), // -SC_DTCT
    // ------- GPIO Group GPP_G -------
    cfg::not_connected(pad(Group::G, 0), Pull::None),
    cfg::not_connected(pad(Group::G, 1), Pull::None),
    cfg::not_connected(pad(Group::G, 2), Pull::None),
    cfg::not_connected(pad(Group::G, 3), Pull::None),
    cfg::output(pad(Group::G, 4), true, Reset::Platform), // TBT_RTD3_PWR_EN
    cfg::output(pad(Group::G, 5), true, Reset::Platform), // TBT_FORCE_USB_PWR
    cfg::output(pad(Group::G, 6), true, Reset::Platform), // -TBT_PERST
    cfg::interrupt(
        pad(Group::G, 7),
        Pull::None,
        Reset::Deep,
        Trigger::Level,
        true,
        Route::Sci,
    ), // -TBT_PCIE_WAKE
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_table_covers_all_sunrise_point_lp_pads_uniquely() {
        assert_eq!(PAD_TABLE.len(), 164);
        let mut seen = [false; 164];
        for entry in PAD_TABLE {
            let number = entry.pad.number_for(Kind::Lp).unwrap() as usize;
            assert!(!seen[number], "duplicate LP pad number {number}");
            seen[number] = true;
        }
        assert!(seen.into_iter().all(core::convert::identity));
    }
}
