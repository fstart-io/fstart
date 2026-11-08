// SPDX-License-Identifier: MIT OR Apache-2.0
//! Active OEM `$A10`, NOT the generic Sunstone CPM table. OemPei copies
//! from VA ffda8e5c (36 live rows + pin255 terminator), applied through the
//! Songshan bank with the Rs CPM setting encoding. AmdCpmInitPeim remaps only
//! die != 0 logical pins; die-0 pins below 256 are the local FCH GPIO
//! numbers. Logical pin 265 is the alternate bank (`0x1200`), pin 9.
//! Signal names and the board's DDR5 SPD/PMIC/I3C muxes are not established.

pub const CPM_GPIO: &[(u16, u16)] = &[
    (0, 0x888),
    (1, 0x888),
    (2, 0x888),
    (3, 0x988),
    (4, 0x988),
    (5, 0x881),
    (6, 0x881),
    (7, 0x881),
    (12, 0x888),
    (13, 0x888),
    (14, 0x888),
    (16, 0x982),
    (17, 0x982),
    (19, 0x888),
    (20, 0x888),
    (24, 0x888),
    (26, 0x888),
    (76, 0x988),
    (87, 0x882),
    (88, 0x882),
    (89, 0x988),
    (104, 0x882),
    (105, 0x882),
    (107, 0x881),
    (109, 0x882),
    (115, 0x881),
    (116, 0x881),
    (126, 0x982),
    (135, 0xa81),
    (136, 0x888),
    (137, 0xa81),
    (138, 0x888),
    (139, 0x982),
    (141, 0x981),
    (142, 0x981),
    (265, 0x980),
];

/// NFX OemPei `ffb027b2` rows at `ffb038c4`: (slot-variable offset, logical
/// pin, CPM setting). After the CPM table, the vendor drives each MCIO
/// connector-mode pad from its connector's Setup bifurcation: low (GPIO,
/// pull-down) for every PCIe choice, high (`| 0x41`) only for the hidden
/// SATA templates 13/18. Left as pulled-up inputs, MCIO0/1 and MCIO2/3 fail
/// MPIO training (LINK_TRAINING_FAILURE).
const CONNECTOR_MODE: [(usize, u16, u16); 3] = [(4, 21, 0x8c0), (4, 22, 0x8c0), (5, 106, 0x8c0)];

/// The connector-mode pads for `options`, in vendor order.
pub fn connector_mode(options: &crate::mpio::Options) -> [(u16, u16); 3] {
    let setup = options.slot_variable();
    CONNECTOR_MODE.map(|(offset, pin, setting)| {
        let sata = matches!(setup[offset], 13 | 18);
        (pin, if sata { setting | 0x41 } else { setting })
    })
}

/// Exact NFX FchPerstDeassertGpio sequence (FFA46748..FFA46753), not a
/// guessed translation of CPM_GPIO. No other logical pad is programmed.
/// # Safety
/// Single-socket NFX, decoded local FCH ACPI MMIO, BSP-exclusive access and
/// completed MPIO PERST phase. Caller supplies the post-release delay.
pub unsafe fn release_perst() -> Result<(), &'static str> {
    use fstart_driver_amd::fch::{self, Bus, NativeBus, Reg, gpio::Control};
    use tock_registers::interfaces::{Readable, Writeable};
    let bus = unsafe { NativeBus::<false>::without_config() };
    let registers =
        [0x1568, 0x1228].map(|offset| Reg::<u32, Control::Register>::mmio(fch::ACPI_MMIO + offset));
    for reg in registers {
        if bus.reg(reg).get() == u32::MAX {
            return Err("PERST inaccessible");
        }
    }
    let value = Control::OUTPUT::SET + Control::OUTPUT_ENABLE::SET + Control::DRIVE_STRENGTH.val(2);
    for reg in registers {
        bus.reg(reg).write(value);
        if bus.reg(reg).get() & 0x00c6_0000 != 0x00c4_0000 {
            return Err("PERST readback");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connector_mode_pads_follow_the_mcio_setup_bytes() {
        let pcie = connector_mode(&crate::mpio::Options::DEFAULT);
        assert_eq!(pcie, [(21, 0x8c0), (22, 0x8c0), (106, 0x8c0)]);
        // Only the hidden SATA templates drive a connector's pads high.
        assert!(
            CONNECTOR_MODE
                .iter()
                .all(|&(offset, ..)| offset == 4 || offset == 5)
        );
    }
    #[test]
    fn active_rows_not_allocated_capacity_or_terminator() {
        assert_eq!(CPM_GPIO.len(), 36);
        assert!(!CPM_GPIO.iter().any(|&(pin, _)| pin == 255));
        assert_eq!(CPM_GPIO.last(), Some(&(265, 0x980)));
        for (i, &(pin, _)) in CPM_GPIO.iter().enumerate() {
            assert!(!CPM_GPIO[..i].iter().any(|&(other, _)| other == pin));
        }
    }
}
