// SPDX-License-Identifier: MIT OR Apache-2.0
//! Source-backed Picasso ACPIMMIO and always-on device power control.
use crate::picasso::ACPI_MMIO;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite},
};
register_bitfields![u8,
    AoacControl [TARGET OFFSET(0) NUMBITS(2) [Initialized=1], POWER OFFSET(3) NUMBITS(1) []],
    AoacState [POWER_RESET OFFSET(0) NUMBITS(1) [], REFERENCE_CLOCK OFFSET(1) NUMBITS(1) []]
];
register_bitfields![u32,
    Decode [LEGACY OFFSET(0) NUMBITS(1) [], CF9 OFFSET(1) NUMBITS(1) [],
        DMA OFFSET(2) NUMBITS(1) [], DMA80 OFFSET(3) NUMBITS(1) [],
        IOAPIC OFFSET(5) NUMBITS(1) [], HPET OFFSET(6) NUMBITS(1) [],
        HPET64 OFFSET(28) NUMBITS(1) [], HPET_MSI OFFSET(29) NUMBITS(1) []]
];
register_structs! {
    Pm {
        (0x00 => decode: ReadWrite<u32, Decode::Register>),
        (0x04 => _reserved0),
        (0x60 => event: ReadWrite<u16>),
        (0x62 => control: ReadWrite<u16>),
        (0x64 => timer: ReadWrite<u16>),
        (0x66 => _reserved1),
        (0x68 => gpe: ReadWrite<u16>),
        (0x6a => command: ReadWrite<u16>),
        (0x6c => config: ReadWrite<u8>),
        (0x6d => _reserved2),
        (0xbe => reset_control: ReadWrite<u8>),
        (0xbf => @END),
    },
    AoacDevice {
        (0 => control: ReadWrite<u8, AoacControl::Register>),
        (1 => state: ReadOnly<u8, AoacState::Register>),
        (2 => @END),
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidDevice,
    PowerTimeout(u8),
}

pub struct Fch;
impl Fch {
    /// # Safety
    /// ACPIMMIO is decoded and UC-mapped, no concurrent FCH programming.
    pub unsafe fn new() -> Self {
        Self
    }
    fn pm(&self) -> &Pm {
        unsafe { &*((ACPI_MMIO + 0x300) as *const Pm) }
    }
    fn device(&self, device: u8) -> Result<&AoacDevice, Error> {
        if !matches!(device, 0 | 7..=9 | 11..=12 | 16..=17 | 26..=28) {
            return Err(Error::InvalidDevice);
        }
        Ok(unsafe { &*((ACPI_MMIO + 0x1e40 + usize::from(device) * 2) as *const AoacDevice) })
    }
    /// Start all devices before waiting. Polling is bounded;
    /// the caller supplies a real 100-us delay, not a CPU-speed-dependent spin.
    pub fn power_devices(
        &mut self,
        devices: &[u8],
        polls: u32,
        mut delay_100us: impl FnMut(),
    ) -> Result<(), Error> {
        for &id in devices {
            self.device(id)?;
        }
        for &id in devices {
            self.device(id)?
                .control
                .modify(AoacControl::POWER::SET + AoacControl::TARGET::Initialized);
        }
        for &id in devices {
            let register = self.device(id)?;
            let ready = || {
                register
                    .state
                    .matches_all(AoacState::POWER_RESET::SET + AoacState::REFERENCE_CLOCK::SET)
            };
            let mut remaining = polls;
            while !ready() {
                if remaining == 0 {
                    return Err(Error::PowerTimeout(id));
                }
                delay_100us();
                remaining -= 1;
            }
        }
        Ok(())
    }
    pub fn clear_legacy_uart_decode(&mut self) {
        // Range_0 defaults enabled. No legacy aliases for the MMIO UART.
        unsafe { &*(0xfedc0020 as *const ReadWrite<u16>) }.set(0);
    }
    pub fn enable_legacy_decode(&mut self) {
        self.pm()
            .decode
            .modify(Decode::LEGACY::SET + Decode::CF9::SET);
    }
    pub fn disable_legacy_dma(&mut self) {
        self.pm()
            .decode
            .modify(Decode::DMA::CLEAR + Decode::DMA80::CLEAR);
    }
    pub fn enable_hpet_ioapic(&mut self) {
        self.pm().decode.modify(
            Decode::IOAPIC::SET + Decode::HPET::SET + Decode::HPET_MSI::SET + Decode::HPET64::CLEAR,
        );
    }
    /// ACPI standard decode only. No SMI routing is enabled without an installed handler.
    pub fn acpi_ports_without_smm(&mut self) {
        let pm = self.pm();
        pm.event.set(0x400);
        pm.control.set(0x404);
        pm.timer.set(0x408);
        pm.gpe.set(0x420);
        pm.command.set(0);
        pm.config.set(0x1 | 0x2 | 0x4 | 0x10);
    }
    pub fn disable_keyboard_reset(&mut self) {
        let register = &self.pm().reset_control;
        register.set(register.get() & !(1 << 4));
    }
}

#[cfg(target_arch = "x86_64")]
/// Enable the decode using indexed BYTE I/O. PMx04 is read/written as FOUR
/// separate bytes, preserving adjacent BIOSRAM/other decode policy.
/// # Safety
/// Ring 0, exclusive access to CD6/CD7. MMIO remains UC during and after this.
pub unsafe fn enable_acpimmio() {
    unsafe fn read(index: u8) -> u8 {
        let value: u8;
        unsafe {
            core::arch::asm!("out dx, al", in("dx") 0xcd6u16, in("al") index, options(nostack));
            core::arch::asm!("in al, dx", in("dx") 0xcd7u16, out("al") value, options(nostack));
        }
        value
    }
    unsafe fn write(index: u8, value: u8) {
        unsafe {
            core::arch::asm!("out dx, al", in("dx") 0xcd6u16, in("al") index, options(nostack));
            core::arch::asm!("out dx, al", in("dx") 0xcd7u16, in("al") value, options(nostack));
        }
    }
    let mut bytes = [0u8; 4];
    for (i, value) in bytes.iter_mut().enumerate() {
        *value = unsafe { read(4 + i as u8) };
    }
    bytes[0] |= 2;
    for (i, value) in bytes.into_iter().enumerate() {
        unsafe { write(4 + i as u8, value) };
    }
}
