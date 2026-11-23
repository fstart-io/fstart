//! Lynx Point sleep preparation, on top of the shared PCH SMI dispatcher.
//!
//! BARs come from the firmware PCI allocator at installation. Never follow a
//! BAR supplied by the OS from SMM; skip any controller moved since boot.

use super::LynxPoint;
use crate::southbridge::smi::{IchSmmConfig, PendingSmi, PrepareError, SmiControl};
use fstart_core::mmio::MmioRegister;
use fstart_pci::{cf8, ecam::EcamDevice};
use tock_registers::interfaces::{Readable, Writeable};

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Config {
    ich: IchSmmConfig,
    lp: bool,
    prepare_ehci: bool,
    gpio_base: u16,
    tsc_hz: u64,
    bars: [u32; 3],
}

impl Config {
    pub(super) fn new(pch: &LynxPoint) -> Self {
        Self {
            ich: pch.smi().smm_handler_config(),
            lp: pch.config.gpio.lp(),
            prepare_ehci: !pch.config.xhci_default,
            gpio_base: pch.config.gpio_base(),
            tsc_hz: fstart_arch::x86::tsc_frequency_hz(),
            bars: [20, 29, 26].map(|slot| {
                EcamDevice::new(0, slot, 0)
                    .memory_bar(0x10)
                    .and_then(|bar| bar.try_into().ok())
                    .unwrap_or(0)
            }),
        }
    }

    /// Configured Lynx Point-LP GPIO controller, if this PCH is LP.
    #[must_use]
    pub const fn lp_gpio(&self) -> Option<super::gpio::LpGpio> {
        if self.lp {
            Some(super::gpio::LpGpio::new(self.gpio_base))
        } else {
            None
        }
    }

    fn prepare_usb(&self, state: u8) {
        if state < 3 {
            return;
        }
        let [xhci, ehci1, ehci2] = self.bars;
        for (i, slot, base) in [(1, 29, ehci1), (2, 26, ehci2), (0, 20, xhci)] {
            if i != 0 && (!self.prepare_ehci || state < 4 || (i == 2 && self.lp)) {
                continue;
            }
            if base == 0 || base > u32::MAX - 0xffff {
                continue;
            }
            // Configuration space in SMM has no stage-local ECAM binding.
            unsafe {
                if cf8::read16(0, slot, 0, 0) != 0x8086
                    || cf8::read32(0, slot, 0, 0x10) & !15 != base
                {
                    continue;
                }
                // All offsets below fit the validated 32-bit span. Do not link
                // panicking address arithmetic into the PIC SMM image.
                let reg32 = |off: u32| MmioRegister::<u32>::new((base + off) as usize);
                let cmd = cf8::read16(0, slot, 0, 4);
                if i == 0 {
                    if cf8::read32(0, slot, 0, 0x14) != 0 {
                        continue;
                    }
                    let power = cf8::read16(0, slot, 0, 0x74);
                    if self.lp {
                        cf8::write16(0, slot, 0, 4, (cmd & !4) | 2);
                        cf8::write16(0, slot, 0, 0x74, power & !0x8003);
                        cf8::write32(0, slot, 0, 0xb0, cf8::read32(0, slot, 0, 0xb0) & !0x6000);
                        let cfg = reg32(0x816c);
                        cfg.set(cfg.get() & !0x4004);
                        let enabled = !cf8::read32(0, slot, 0, 0xe8) & 15;
                        let reg = |n: u32| reg32(0x510 + n * 16);
                        super::usb::recover_ss(
                            4,
                            enabled,
                            false,
                            |n| reg(n).get(),
                            |n, v| reg(n).set(v),
                            |us| fstart_arch::x86::udelay_tsc(us, self.tsc_hz),
                        );
                        let cfg = reg32(0x80e0);
                        cfg.set(cfg.get() | (1 << 15));
                        cf8::write16(0, slot, 0, 4, cmd);
                    }
                    enter_d3_with_pme(
                        || cf8::read16(0, slot, 0, 0x74),
                        |v| cf8::write16(0, slot, 0, 0x74, v),
                    );
                } else if state >= 4 {
                    let power = cf8::read16(0, slot, 0, 0x54);
                    let d3 = power & 3 == 3;
                    if d3 {
                        cf8::write16(0, slot, 0, 4, (cmd & !4) | 2);
                        cf8::write16(0, slot, 0, 0x54, power & !0x8003);
                    } else if cmd & 2 == 0 {
                        continue;
                    }
                    let command = reg32(0x20);
                    let port = reg32(0x64);
                    if command.get() & 1 != 0 {
                        command.set(command.get() & !0x30);
                    }
                    if port.get() & 4 != 0 {
                        if port.get() & 128 == 0 {
                            port.set(port.get() | 128);
                        }
                        fstart_arch::x86::udelay_tsc(25_000, self.tsc_hz);
                        command.set(stop_ehci(command.get()));
                    }
                    if d3 {
                        cf8::write16(0, slot, 0, 4, cmd);
                        cf8::write16(0, slot, 0, 0x54, power & !0x8000);
                    }
                }
            }
        }
    }

    /// Dispatch board EC/APM/GPI policy before completing the chipset's sleep
    /// transition and issuing EOS. Call from the rendezvous owner only.
    ///
    /// # Safety
    /// `self` must be the installer-supplied configuration in SMRAM, and the
    /// caller must own this SMI dispatch.
    pub unsafe fn dispatch(&self, board: impl FnOnce(&PendingSmi) -> Result<(), PrepareError>) {
        // Serialized permanent rendezvous, including the install-time smoke SMI.
        unsafe { fstart_arch::x86::cpu::intel::haswell::lock_smm_fetches() };
        let p = unsafe { PendingSmi::read_sources(&self.ich) };
        if p.apm == Some(0xca) {
            // APM status byte: unsupported runtime switch. Never follow a
            // mutable BAR or enable DMA in response to a payload command.
            // The PIC SMM image cannot call the ramstage console. Log the
            // rejected operation to the firmware POST trace instead.
            unsafe {
                fstart_core::pio::outb(0x80, 0xca);
                fstart_core::pio::outb(0xb3, 0xff);
            }
        }
        let prepared = board(&p);
        p.complete(prepared, |state| self.prepare_usb(state));
    }
}

/// xHCI PCI `0x74` (16 bits): do not consume `PME_STS` on the D3hot write.
#[inline(always)]
fn enter_d3_with_pme(read: impl Fn() -> u16, mut write: impl FnMut(u16)) {
    for (clear, set) in [(0x8000, 3), (0, 0x8000), (0x8000, 0x100)] {
        write((read() & !clear) | set);
    }
}

#[inline(always)]
fn stop_ehci(command: u32) -> u32 {
    command & !1
}

/// Default handler for boards without an EC hook.
pub fn handle(_ctx: &mut fstart_smm::SmmContext<'_>, c: &Config) {
    unsafe { c.dispatch(|_| Ok(())) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntelSouthbridgeDriver;
    use crate::lynxpoint::{Gpios, LynxPointConfig, gpio};
    use crate::southbridge::{gpio_ich::GpioConfig, smi};

    #[test]
    fn d3_then_ack_then_arm() {
        let pm = core::cell::Cell::new(0xc000u16);
        let mut writes = alloc::vec::Vec::new();
        enter_d3_with_pme(
            || pm.get(),
            |v| {
                writes.push(v);
                let status = pm.get() & !v & 0x8000;
                pm.set((v & !0x8000) | status);
            },
        );
        assert_eq!(writes, [0x4003, 0xc003, 0x4103]);
    }

    #[test]
    fn stop_preserves_command_except_run() {
        assert_eq!(stop_ehci(0xab81), 0xab80);
    }

    #[test]
    fn smi_config_passes_configured_gpio_base_for_lp_and_h() {
        static LP_PADS: gpio::PadTable = [gpio::Pad::new(gpio::Mode::Unused); 95];
        static LP_CFG: LynxPointConfig = LynxPointConfig::new(Gpios::Lp(&LP_PADS));
        let lp = LynxPoint::new_from_config(&LP_CFG).unwrap();
        assert_eq!(
            lp.smi().smm_handler_config(),
            smi::IchSmi::new(0x1000, smi::LYNXPOINT_LP_GPE0)
                .with_sleep_smi(0x1400)
                .smm_handler_config()
        );

        static H_GPIO: GpioConfig = GpioConfig::new();
        static H_CFG: LynxPointConfig = LynxPointConfig::new(Gpios::Traditional(&H_GPIO));
        let h = LynxPoint::new_from_config(&H_CFG).unwrap();
        assert_eq!(
            h.smi().smm_handler_config(),
            smi::IchSmi::new(0x500, smi::LYNXPOINT_GPE0)
                .with_sleep_smi(0x480)
                .smm_handler_config()
        );
    }
}
