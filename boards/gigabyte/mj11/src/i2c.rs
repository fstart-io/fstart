// SPDX-License-Identifier: MIT OR Apache-2.0
//! The devices on FCH I2C3: the VR controllers (vendor GbtIRPower) and, on
//! the EC1, the PCIe re-drivers of the riser and backplane (GbtReDriverPei).
use crate::variant::{I2cRmw, VrImage};
use embedded_hal::{delay::DelayNs, i2c::I2c};

/// I2C3, on GPIO19/20 (IOMUX function 1).
pub const BUS: usize = 3;
pub const PADS: [u8; 2] = [19, 20];
pub const FUNCTION: u8 = 1;

const VR_MUX_A: u8 = 0x73;
const VR_MUX_B: u8 = 0x71;
const VR_WRITE_TRIES: usize = 3;

/// The vendor VR code waits 10 us after every successful VR register access.
struct Vr<'a, I, D> {
    i2c: &'a mut I,
    delay: D,
    address: u8,
}
impl<I: I2c, D: DelayNs> Vr<'_, I, D> {
    fn read(&mut self, reg: u8) -> Result<u8, I::Error> {
        let mut value = [0];
        self.i2c.write_read(self.address, &[reg], &mut value)?;
        self.delay.delay_us(10);
        Ok(value[0])
    }
    fn write(&mut self, reg: u8, value: u8) -> Result<(), I::Error> {
        self.i2c.write(self.address, &[reg, value])?;
        self.delay.delay_us(10);
        Ok(())
    }
    /// Write every register that differs, verifying each write.
    fn load(&mut self, image: &VrImage) -> Result<(), ()> {
        image.regs.iter().try_for_each(|r| {
            let want = r.value & r.mask;
            let mut value = self.read(r.reg).map_err(drop)?;
            for _ in 0..VR_WRITE_TRIES {
                if value & r.mask == want {
                    return Ok(());
                }
                self.write(r.reg, want).map_err(drop)?;
                value = self.read(r.reg).map_err(drop)?;
            }
            if value & r.mask == want { Ok(()) } else { Err(()) }
        })
    }
}

/// A PCA954x: the control byte goes after a "register" 0, as in the vendor
/// code.
pub(crate) fn select<I: I2c>(i2c: &mut I, mux: u8, channels: u8) -> Result<(), I::Error> {
    i2c.write(mux, &[0, channels])
}

/// Load the VR register images. Lock write errors are ignored, as in the
/// vendor code; a failed image stops the rest.
pub fn program_vrs<I: I2c, D: DelayNs + Copy>(i2c: &mut I, delay: D, images: &[VrImage]) {
    let _ = select(i2c, VR_MUX_A, 0);
    for image in images {
        if select(i2c, VR_MUX_B, 1 << image.mux_channel).is_err() {
            continue;
        }
        let mut vr = Vr {
            i2c: &mut *i2c,
            delay,
            address: image.address,
        };
        let _ = vr.write(image.lock_reg, 0);
        let loaded = vr.load(image);
        if loaded.is_ok() {
            let _ = vr.write(image.lock_reg, 7);
            fstart_log::debug!("VR {}: programmed", fstart_log::Hex(image.address.into()));
        } else {
            fstart_log::error!("VR {}: programming failed", fstart_log::Hex(image.address.into()));
            break;
        }
    }
    let _ = select(i2c, VR_MUX_B, 1);
}

const fn is_mux(address: u8) -> bool {
    matches!(address, 0x70..=0x77)
}

/// Ordered mux selection and lane tuning of the external re-drivers. They
/// sit on the riser and backplane of the G431 chassis; without them the
/// downstream mux doesn't answer, so skip the accesses behind a mux that
/// failed to switch.
pub fn program_redrivers<I: I2c>(i2c: &mut I, writes: &[I2cRmw]) {
    let (_, failures, skipped) =
        writes
            .iter()
            .fold((true, 0u32, 0u32), |(path, failures, skipped), w| {
                let mux = is_mux(w.address);
                if !mux && !path {
                    return (path, failures, skipped + 1);
                }
                let mut value = [0];
                let ok = i2c.write_read(w.address, &[w.reg], &mut value).is_ok()
                    && i2c
                        .write(w.address, &[w.reg, (value[0] & w.and_mask) | w.or_mask])
                        .is_ok();
                // Only a channel selection (a nonzero value) opens a path.
                let path = if mux && w.or_mask != 0 { ok } else { path };
                (path, failures + u32::from(!ok), skipped)
            });
    if failures != 0 || skipped != 0 {
        fstart_log::info!(
            "External re-drivers: {} accesses failed, {} skipped",
            failures,
            skipped
        );
    }
}
