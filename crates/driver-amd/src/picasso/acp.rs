// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native ACP shutdown before DXIO.
use super::registers::Smn;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    pub register: u32,
    pub value: u32,
    pub mask: u32,
    pub expected: u32,
}
fn wait(
    smn: &mut impl Smn,
    reg: u32,
    mask: u32,
    expected: u32,
    delay_us: &mut impl FnMut(u32),
) -> Result<(), Error> {
    let mut value = 0;
    for _ in 0..1000 {
        value = smn.read(reg);
        if value == u32::MAX {
            break;
        }
        if value & mask == expected {
            return Ok(());
        }
        delay_us(1);
    }
    Err(Error {
        register: reg,
        value,
        mask,
        expected,
    })
}
/// Disabled ACP must still be powered and taken out of reset to shut down
/// its SRAM. A timeout never proceeds to hide the unquiesced function.
pub fn power_off(
    smn: &mut impl Smn,
    enabled: bool,
    delay_us: &mut impl FnMut(u32),
) -> Result<(), Error> {
    if enabled {
        return Ok(());
    }
    smn.update(0x0124141c, 0, 1);
    wait(smn, 0x01241420, 3, 0, delay_us)?;
    smn.update(0x01241004, 0, 1);
    smn.update(0x01241000, 0, 1);
    wait(smn, 0x01241000, 1 << 16, 1 << 16, delay_us)?;
    smn.write(0x01241000, 0);
    wait(smn, 0x01241000, 0x10001, 0, delay_us)?;
    smn.update(0x01241010, 0xffff, 0x1ff);
    smn.update(0x012418c0, 0x0fff0000, 0x0fff0000);
    wait(smn, 0x012418cc, 0xfff, 0, delay_us)?;
    smn.write(0x01241010, 0);
    smn.write(0x01241004, 0);
    smn.update(0x10134a00, 1 << 28, 0);
    Ok(())
}
