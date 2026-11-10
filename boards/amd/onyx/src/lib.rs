// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Onyx: single-socket SP5/Genoa reference board, wired as AMD's public
//! reference design; straps and installed cards may differ per board.
#![cfg_attr(not(test), no_std)]
#[cfg(feature = "smm")]
pub mod smm;
pub mod wiring;

pub struct Board;
impl fstart_platform_rs::Board for Board {
    type Soc = fstart_platform_rs::Genoa;
    const NAME: &'static str = "AMD Onyx";
    const SMBIOS_MANUFACTURER: &'static str = "AMD";
    const SMBIOS_PRODUCT: &'static str = "Onyx";
    // 9004 reporting ceiling: 256-GiB RDIMM per position (AMD memory
    // population guide). This is not installed capacity or a DIMM QVL.
    const SMBIOS_MAX_DIMM_MIB: u32 = 256 * 1024;
    /// All twelve UMC channels have DIMM slots.
    const MEMORY_CHANNELS: u16 = 0xfff;
    /// 32 MiB host NOR.
    const FLASH_SIZE: u32 = 0x200_0000;
    // Public Onyx coreboot Kconfig explicitly chooses SoC UART1 for BMC SOL.
    // Do not run the Tyan AST2600 Super-I/O COM1 script on this board.
    const CONSOLE: fstart_platform_rs::Console =
        fstart_platform_rs::Console::Fch(fstart_platform_rs::Uart::Uart1);
    /// openSIL's Onyx configuration (NbioGlobalCgOverride 0Fh): IOHC, SST
    /// and NBIF clock gating and TPH completion on, SYSHUB gating off.
    const NBIO: fstart_driver_amd::nbio::rs::Policy = fstart_driver_amd::nbio::rs::Policy {
        clock_gating: true,
        syshub_clock_gating: false,
        tph_completer: true,
    };
    const LINUX_BOOTARGS: &'static str = "earlycon=uart8250,mmio32,0xfedca000 console=ttyS0,115200n8 keep_bootcon ignore_loglevel loglevel=8 rdinit=/init panic=-1";
    // Factory snapshot: lane135 trains the onboard Broadcom BCM5720;
    // lane134's ASPEED bridge is preserved outside normal MPIO requests.
    const REQUIRED_PCIE: &'static [(u8, u16)] = &[(135, 0x14e4)];
    const RETAINED_PCIE: &'static [(u8, u16, u16)] = &[(0, 0x1a03, 0x1150), (0, 0x1a03, 0x2000)];
    const PCI_ROOT_MMIO_SIZE: u64 = 0x8000000;
    const PRIMARY_VGA: Option<(u16, u16)> = Some((0x1a03, 0x2000));
    // Onyx_SilCfg: CONFIG_MPIO_SKIP_PSP_MSG=1, unlike Siena's vendor firmware.
    const MPIO_PSP_VETTING: bool = false;
    const MPIO_ENCODING: fstart_driver_amd::mpio::mailbox::Encoding =
        fstart_driver_amd::mpio::mailbox::Encoding::LowByte;
    // The SMU revision the USB policy (absolute interface) is for: this
    // firmware rejects the relative-register API.
    const USB_SMU_VERSION: Option<u32> = Some(0x0447_5b00);
    fn mpio_ports() -> Result<
        heapless::Vec<fstart_driver_amd::mpio::session::Config<'static>, 32>,
        fstart_driver_amd::mpio::session::Error,
    > {
        heapless::Vec::from_slice(&wiring::PCIE)
            .map_err(|_| fstart_driver_amd::mpio::session::Error::Capacity)
    }
    fn usb_policy() -> Option<fstart_driver_amd::fch::songshan::usb::Policy> {
        Some(wiring::USB)
    }
    unsafe fn release_perst() -> Result<(), &'static str> {
        use fstart_driver_amd::fch::{Acpimmio, Fch, NativeBus};
        let bus = unsafe { NativeBus::<false>::without_config() };
        wiring::release_perst(&Fch::new(&bus, Acpimmio::DIE0))
    }
}

#[cfg(test)]
mod tests {
    use fstart_platform_rs::{Board as _, Console, Uart};
    #[test]
    fn console_is_the_soc_uart_not_a_superio_pio_alias() {
        assert!(matches!(super::Board::CONSOLE, Console::Fch(Uart::Uart1)));
        assert_eq!(Uart::Uart1.base(), 0xfedc_a000);
        assert_eq!(super::Board::BAUD, 115_200);
        assert!(!super::Board::LEGACY_DEVICES);
    }
}
