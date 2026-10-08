use crate::Board;
use fstart_platform_intel::IntelBoard;

impl IntelBoard for Board {
    #[cfg(fstart_stage_env = "car")]
    type EarlyHooks = crate::mainboard::X200Mainboard;
    #[cfg(fstart_stage_env = "ram")]
    type MainstageHooks = crate::mainboard::X200Mainboard;
    type Console = fstart_driver_usbdebug::EhciDebug;

    fn console_config() -> fstart_driver_usbdebug::Config {
        crate::config::EHCI_CONSOLE
    }
    fn console_node() -> &'static str {
        "ich9/ehci2/debug"
    }
    #[cfg(fstart_stage_env = "ram")]
    fn smbios_identity() -> &'static fstart_platform_intel::tables::SmbiosIdentity<'static> {
        &crate::mainboard::IDENTITY
    }
}
