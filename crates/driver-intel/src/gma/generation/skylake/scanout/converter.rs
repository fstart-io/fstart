// SPDX-License-Identifier: MIT OR Apache-2.0
//! DP++ level-shifter/protocol converters (libgfxinit DP_Dual_Mode).

use crate::gma::dp_aux::{AuxRegs, read_i2c_registers, write_i2c_register};
use crate::gma::error::GmaError;
use crate::gma::mmio::{Mmio, delay_us};
use crate::gma::types::Port;

const ADDRESS: u8 = 0x40;
const ID: &[u8; 16] = b"DP-HDMI ADAPTOR\x04";
const ADAPTOR_ID: u8 = 0x10;
const LSPCON_ID: u8 = 0xa8;
const CHANGE_MODE: u8 = 0x40;
const CURRENT_MODE: u8 = 0x41;
const PCON: u8 = 1;

pub(super) fn prepare(mmio: &Mmio, port: Port) -> Result<(), GmaError> {
    let aux = AuxRegs::gen9_for_port(port)?;
    configure(
        |offset, out| read_i2c_registers(mmio, aux, ADDRESS, offset, out),
        |offset, value| write_i2c_register(mmio, aux, ADDRESS, offset, value),
        delay_us,
    )
}

fn configure(
    mut read: impl FnMut(u8, &mut [u8]) -> Result<(), GmaError>,
    mut write: impl FnMut(u8, u8) -> Result<(), GmaError>,
    mut delay: impl FnMut(u32),
) -> Result<(), GmaError> {
    let mut id = [0; 16];
    let mut adapter = [0];
    // A native DP sink and passive DP++ wiring do not expose a converter.
    // Do not write until both identification reads confirm an LSPCON.
    if read(0, &mut id).is_err()
        || &id != ID
        || read(ADAPTOR_ID, &mut adapter).is_err()
        || adapter != [LSPCON_ID]
    {
        return Ok(());
    }
    let mut mode = [0];
    if read(CURRENT_MODE, &mut mode).is_ok() && mode == [PCON] {
        return Ok(());
    }
    let mut changed = false;
    for _ in 0..6 {
        if write(CHANGE_MODE, PCON).is_ok() {
            changed = true;
            break;
        }
        delay(500);
    }
    if !changed {
        return Err(GmaError::HardwareError);
    }
    // The converter can NAK while rebooting into protocol-converter mode.
    for _ in 0..200 {
        delay(10_000);
        if read(CURRENT_MODE, &mut mode).is_ok() && mode == [PCON] {
            return Ok(());
        }
    }
    Err(GmaError::Timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};

    #[test]
    fn converter_must_identify_before_writes_and_can_nak_during_switch() {
        for (adapter, acknowledgements, expected_writes, expected) in [
            (0xa0, 0, 0, Ok(())), // passive HDMI
            (LSPCON_ID, 2, 1, Ok(())),
            (LSPCON_ID, 201, 1, Err(GmaError::Timeout)),
        ] {
            let polls = Cell::new(0);
            let writes = RefCell::new(alloc::vec::Vec::new());
            let result = configure(
                |offset, out| match offset {
                    0 => {
                        out.copy_from_slice(ID);
                        Ok(())
                    }
                    ADAPTOR_ID => {
                        out[0] = adapter;
                        Ok(())
                    }
                    CURRENT_MODE => {
                        let n = polls.get();
                        polls.set(n + 1);
                        match n {
                            0 => {
                                out[0] = 0;
                                Ok(())
                            }
                            n if n > acknowledgements => {
                                out[0] = PCON;
                                Ok(())
                            }
                            _ => Err(GmaError::HardwareError),
                        }
                    }
                    _ => unreachable!(),
                },
                |offset, val| {
                    writes.borrow_mut().push((offset, val));
                    Ok(())
                },
                |_| {},
            );
            assert_eq!(result, expected);
            assert_eq!(&*writes.borrow(), &[(CHANGE_MODE, PCON)][..expected_writes]);
        }
    }
}
