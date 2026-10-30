//! ACPI description of the FCH devices fstart leaves running: the AMBA
//! UARTs (`AMDI0020`) or their legacy COM aliases (`PNP0501`), the I2C
//! controllers (`AMDI0010`), the GPIO controller (`AMDI0030`) and the ISA
//! legacy devices under the LPC bridge. Shared by Taishan (Naples) and
//! Huashan (Rome, Milan).
//!
//! Everything is read back from the hardware ([`FchAcpi::read`]) instead of
//! being restated: a controller is described when its DesignWare component
//! register answers (it is powered and decoded), its interrupt is the
//! PCI_INTR IOAPIC route fstart programmed, and a UART that the legacy
//! decode maps to a COM port is described as that port only. The donor
//! DSDTs (ACPI_SMM §3.7) carry the same objects with CRB-static IRQs that
//! disagree with the routes their own FCH code programs; HIDs, MMIO and
//! DMA blocks are taken from them as facts. PCI_INTR indices are public
//! (AMD PPR 55570 Table 71).

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::aml_linker::device_vec;
use fstart_acpi::{AmlError, BoundAmlFragment, acpi_dsl};
use heapless::Vec as HVec;

use super::{Window, irq, uart};
use crate::fch::aml::GPIO_BASE;

/// DesignWare component type register (`*_COMP_TYPE`, offset 0xFC); the
/// high half reads `"DW"` on a powered DesignWare block.
const DW_COMP_TYPE: u32 = 0xFC;
const DW_SIGNATURE: u32 = 0x4457;

/// PCI_INTR IOAPIC indices (PPR 55570 Table 71 index | 0x80).
const UART_INTR: [u8; 4] = [0xF4, 0xF5, 0xF8, 0xF9];
const I2C_INTR: [u8; 6] = [0xF0, 0xF1, 0xF2, 0xF3, 0xF6, 0xF7];
const GPIO_INTR: u8 = 0xE2;

/// UART `n`: MMIO block and its DMA block.
const UART_MMIO: [(u32, u32); 4] = [
    (0xFEDC_9000, 0xFEDC_7000),
    (0xFEDC_A000, 0xFEDC_8000),
    (0xFEDC_E000, 0xFEDC_C000),
    (0xFEDC_F000, 0xFEDC_D000),
];
const I2C_MMIO: [u32; 6] = [
    0xFEDC_2000,
    0xFEDC_3000,
    0xFEDC_4000,
    0xFEDC_5000,
    0xFEDC_6000,
    0xFEDC_B000,
];
/// The GPIO banks' extent from `fch::aml::GPIO_BASE`.
const GPIO_LENGTH: u32 = 0x300;

/// The legacy ranges of [`uart::LEGACY_DECODE`]: bit `k` enables range `k`,
/// bits `8 + 2k` select the UART behind it (donor `CKST`/`COMn`).
const LEGACY_PORTS: [u16; 4] = [0x2E8, 0x2F8, 0x3E8, 0x3F8];
/// COM number of each legacy range (`_UID`/`_DDN`).
const LEGACY_COM: [u8; 4] = [4, 2, 3, 1];

/// A UART described by its MMIO (`AMDI0020`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uart {
    pub index: u8,
    pub irq: u8,
}

/// A UART reached through a legacy COM range (`PNP0501`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyCom {
    pub com: u8,
    pub port: u16,
    pub irq: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I2c {
    pub index: u8,
    pub irq: u8,
}

/// What the FCH contributes to the DSDT.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FchAcpi {
    pub uarts: HVec<Uart, 4>,
    pub legacy: HVec<LegacyCom, 4>,
    pub i2c: HVec<I2c, 6>,
    /// GPIO controller interrupt (route of index 0xE2); `None` without a
    /// route.
    pub gpio_irq: Option<u8>,
}

/// Serialise a bound fragment, reporting a bad operand instead of panicking.
fn emit<const N: usize, const K: usize>(f: BoundAmlFragment<N, K>) -> Result<Vec<u8>, AmlError> {
    let mut v = Vec::new();
    f.emit(&mut v)?;
    Ok(v)
}

fn dw_present<M: Window>(mmio: &M, base: u32) -> bool {
    mmio.read32(base + DW_COMP_TYPE) >> 16 == DW_SIGNATURE
}

/// An ISA IRQ from a PCI_INTR route; 0x1F (and anything above 15) is
/// unrouted.
fn route<I: Window>(io: &I, index: u8) -> Option<u8> {
    Some(irq::read(io, index)).filter(|&r| r <= 15)
}

impl FchAcpi {
    /// Read the state the FCH tasks left: `mmio` reaches the physical FCH
    /// MMIO (`FEDCxxxx`, `FED8xxxx`), `io` the PCI_INTR ports.
    pub fn read<M: Window, I: Window>(mmio: &M, io: &I) -> Self {
        let decode = mmio.read16(uart::LEGACY_DECODE);
        let behind = |k: usize| (decode >> (8 + 2 * k)) as u8 & 3;
        let legacy_ranges = || (0..4).filter(move |&k| decode & (1 << k) != 0);
        let uart_irq = |n: u8| route(io, UART_INTR[usize::from(n)]);
        let mapped = |n: u8| legacy_ranges().any(|k| behind(k) == n);
        let powered = |n: u8| dw_present(mmio, UART_MMIO[usize::from(n)].0);
        Self {
            uarts: (0..4u8)
                .filter(|&n| powered(n) && !mapped(n))
                .filter_map(|n| uart_irq(n).map(|irq| Uart { index: n, irq }))
                .collect(),
            legacy: legacy_ranges()
                .filter(|&k| powered(behind(k)))
                .filter_map(|k| {
                    uart_irq(behind(k)).map(|irq| LegacyCom {
                        com: LEGACY_COM[k],
                        port: LEGACY_PORTS[k],
                        irq,
                    })
                })
                .collect(),
            i2c: (0..6u8)
                .filter(|&n| dw_present(mmio, I2C_MMIO[usize::from(n)]))
                .filter_map(|n| {
                    route(io, I2C_INTR[usize::from(n)]).map(|irq| I2c { index: n, irq })
                })
                .collect(),
            gpio_irq: route(io, GPIO_INTR),
        }
    }

    /// Devices for `\_SB`: `FURn`/`COMn`, `I2Cx`, `GPIO`.
    pub fn sb_aml(&self) -> Result<Vec<u8>, AmlError> {
        let mut out = Vec::new();
        for u in &self.uarts {
            let (mmio, dma) = UART_MMIO[usize::from(u.index)];
            let body = emit(acpi_dsl! {
                Name("_HID", "AMDI0020");
                Name("_UID", #{byte u.index});
                Name("_DSD", Package(
                    ToUUID("daffd814-6eba-4d8c-8a91-bc9bbf4aa301"),
                    Package(
                        Package("clock-frequency", #{const dword uart::MMIO_CLOCK_HZ}),
                        Package("reg-shift", 2u8),
                        Package("reg-io-width", 1u8),
                    )
                ));
                Name("_CRS", ResourceTemplate {
                    Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, #{dword u.irq});
                    Memory32Fixed(ReadWrite, #{dword mmio}, 0x1000u32);
                    Memory32Fixed(ReadWrite, #{dword dma}, 0x1000u32);
                });
            })?;
            out.extend(device_vec(&name("FUR", b'0' + u.index), &body)?);
        }
        for c in &self.legacy {
            let body = emit(acpi_dsl! {
                Name("_HID", EisaId("PNP0501"));
                Name("_UID", #{byte c.com});
                Name("_CRS", ResourceTemplate {
                    IO(#{word c.port}, #{word c.port}, 1u8, 8u8);
                    Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, #{dword c.irq});
                });
            })?;
            out.extend(device_vec(&name("COM", b'0' + c.com), &body)?);
        }
        for i in &self.i2c {
            out.extend(crate::fch::aml::i2c(
                &name("I2C", b'A' + i.index),
                i.index,
                I2C_MMIO[usize::from(i.index)],
                i.irq,
                &[],
            )?);
        }
        if let Some(irq) = self.gpio_irq {
            out.extend(crate::fch::aml::gpio(irq, GPIO_LENGTH)?);
        }
        Ok(out)
    }
}

/// `prefix` plus one character (`FUR0`, `COM1`, `I2CA`).
fn name(prefix: &str, last: u8) -> alloc::string::String {
    let mut s = alloc::string::String::from(prefix);
    s.push(char::from(last));
    s
}

/// FCH I/O ports that no device object claims, `(port, length)`: the
/// legacy ranges, PCI_INTR, PMIO/SMBus index pairs, SMBus and ASF (donor
/// `S900`, which lists the same set; 0x60/0x64 have length 0 there).
pub const SYSTEM_IO: &[(u16, u8)] = &[
    (0x10, 0x10),
    (0x22, 0x1E),
    (0x63, 1),
    (0x65, 1),
    (0x67, 9),
    (0x72, 0x0E),
    (0x80, 1),
    (0x84, 3),
    (0x88, 1),
    (0x8C, 3),
    (0x90, 0x10),
    (0xA2, 0x1E),
    (0xB1, 1),
    (0xE0, 0x10),
    (0x40B, 1),
    (0x4D6, 1),
    (0x900, 0x10),
    (0x910, 0x10),
    (irq::INDEX as u16, 2),
    (0xC14, 1),
    (0xC50, 3),
    (0xC6C, 1),
    (0xC6F, 1),
    (0xCD0, 0x10),
    (super::smbus::SMBUS_IO as u16, 0x10),
    (super::smbus::ASF_IO as u16, 0x20),
];

/// FCH MMIO that no device object claims, `(base, length)`: the ACPI MMIO
/// block around the GPIO bank the `GPIO` device owns, the `FEDC_0000h`
/// block, the SPI controller and the flash window (donor `S900`).
pub const SYSTEM_MMIO: &[(u32, u32)] = &[
    (0xFED8_0000, GPIO_BASE - 0xFED8_0000),
    (GPIO_BASE + 0x400, 0xFED9_0000 - (GPIO_BASE + 0x400)),
    (0xFEDC_0000, 0x1000),
    (super::spi::BASE, 0x1000),
    (0xFF00_0000, 0x0100_0000),
];

/// ISA legacy devices for the LPC bridge scope: PIC, DMA, PIT, RTC (no
/// IRQ 8: the HPET's legacy replacement owns it), speaker and the HPET.
pub fn lpc_aml(hpet_base: u32) -> Result<Vec<u8>, AmlError> {
    emit(acpi_dsl! {
        Device("PIC") {
            Name("_HID", EisaId("PNP0000"));
            Name("_CRS", ResourceTemplate {
                IO(0x20u16, 0x20u16, 0u8, 2u8);
                IO(0xA0u16, 0xA0u16, 0u8, 2u8);
                IO(0x4D0u16, 0x4D0u16, 0u8, 2u8);
                IRQNoFlags(2u8);
            });
        }
        Device("DMAC") {
            Name("_HID", EisaId("PNP0200"));
            Name("_CRS", ResourceTemplate {
                IO(0x00u16, 0x00u16, 0u8, 0x10u8);
                IO(0x81u16, 0x81u16, 0u8, 3u8);
                IO(0x87u16, 0x87u16, 0u8, 1u8);
                IO(0x89u16, 0x89u16, 0u8, 3u8);
                IO(0x8Fu16, 0x8Fu16, 0u8, 1u8);
                IO(0xC0u16, 0xC0u16, 0u8, 0x20u8);
            });
        }
        Device("TIMR") {
            Name("_HID", EisaId("PNP0100"));
            Name("_CRS", ResourceTemplate {
                IO(0x40u16, 0x40u16, 0u8, 4u8);
                IRQNoFlags(0u8);
            });
        }
        Device("RTC") {
            Name("_HID", EisaId("PNP0B00"));
            Name("_CRS", ResourceTemplate {
                IO(0x70u16, 0x70u16, 0u8, 2u8);
            });
        }
        Device("SPKR") {
            Name("_HID", EisaId("PNP0800"));
            Name("_CRS", ResourceTemplate {
                IO(0x61u16, 0x61u16, 0u8, 1u8);
            });
        }
        Device("HPET") {
            Name("_HID", EisaId("PNP0103"));
            Name("_UID", 0u8);
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadOnly, #{dword hpet_base}, 0x400u32);
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::fch::model::Mem;

    fn hw(decode: u16, powered_uarts: &[u8], powered_i2c: &[u8]) -> (Mem, Mem) {
        let mmio = Mem::default();
        mmio.put(uart::LEGACY_DECODE, 2, decode.into());
        powered_uarts
            .iter()
            .for_each(|&n| mmio.put(UART_MMIO[usize::from(n)].0 + DW_COMP_TYPE, 4, 0x4457_0110));
        powered_i2c
            .iter()
            .for_each(|&n| mmio.put(I2C_MMIO[usize::from(n)] + DW_COMP_TYPE, 4, 0x4457_0140));
        let io = Mem::ports();
        // Rome routes (FCH_SP3 §3.3 af70); everything else unrouted.
        (0..=255u32).for_each(|i| io.set_at(irq::DATA, i, 0x1F));
        [
            (0xE2, 7),
            (0xF0, 10),
            (0xF1, 11),
            (0xF4, 3),
            (0xF5, 4),
            (0xF8, 5),
            (0xF9, 12),
        ]
        .iter()
        .for_each(|&(i, r)| io.set_at(irq::DATA, i, r));
        (mmio, io)
    }

    #[test]
    fn powered_blocks_with_their_programmed_routes() {
        let (mmio, io) = hw(0, &[0, 1], &[0, 1]);
        let f = FchAcpi::read(&mmio, &io);
        assert_eq!(
            f.uarts.as_slice(),
            [Uart { index: 0, irq: 3 }, Uart { index: 1, irq: 4 }]
        );
        assert_eq!(
            f.i2c.as_slice(),
            [I2c { index: 0, irq: 10 }, I2c { index: 1, irq: 11 }]
        );
        assert!(f.legacy.is_empty());
        assert_eq!(f.gpio_irq, Some(7));
        let aml = f.sb_aml().unwrap();
        for n in [b"FUR0", b"FUR1", b"I2CA", b"I2CB", b"GPIO"] {
            assert!(aml.windows(4).any(|w| w == n));
        }
        assert!(!aml.windows(4).any(|w| w == b"FUR2"));
        for property in ["clock-frequency", "reg-shift", "reg-io-width"] {
            assert_eq!(
                aml.windows(property.len())
                    .filter(|w| *w == property.as_bytes())
                    .count(),
                f.uarts.len()
            );
        }
        assert_eq!(
            aml.windows(4)
                .filter(|w| *w == uart::MMIO_CLOCK_HZ.to_le_bytes())
                .count(),
            f.uarts.len()
        );
    }

    #[test]
    fn legacy_decoded_uart_is_only_a_com_port() {
        // Range 3 (3F8h, COM1) on, UART1 behind it.
        let (mmio, io) = hw(1 << 3 | 1 << 14, &[0, 1], &[]);
        let f = FchAcpi::read(&mmio, &io);
        assert_eq!(f.uarts.as_slice(), [Uart { index: 0, irq: 3 }]);
        assert_eq!(
            f.legacy.as_slice(),
            [LegacyCom {
                com: 1,
                port: 0x3F8,
                irq: 4
            }]
        );
        let aml = f.sb_aml().unwrap();
        assert!(aml.windows(4).any(|w| w == b"COM1"));
    }

    #[test]
    fn nothing_powered_gives_only_gpio() {
        let (mmio, io) = hw(0, &[], &[]);
        let f = FchAcpi::read(&mmio, &io);
        assert!(f.uarts.is_empty() && f.i2c.is_empty() && f.legacy.is_empty());
        assert!(lpc_aml(0xFED0_0000).is_ok());
    }

    #[test]
    fn system_mmio_leaves_the_gpio_bank_out() {
        let gpio = u64::from(GPIO_BASE)..u64::from(GPIO_BASE + GPIO_LENGTH);
        assert!(SYSTEM_MMIO.iter().all(|&(b, l)| {
            let (b, l) = (u64::from(b), u64::from(l));
            b + l <= gpio.start || b >= gpio.end
        }));
        assert_eq!(
            SYSTEM_MMIO[..2],
            [(0xFED8_0000, 0x1500), (0xFED8_1900, 0xE700)]
        );
    }
}
