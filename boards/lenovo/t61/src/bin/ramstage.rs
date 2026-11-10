//! T61 devices, CPU power policy, tables and payload handoff.
#![no_std]
#![no_main]
extern crate alloc;
use fstart_board_lenovo_t61::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::ramstage::{Path, Ramstage};
static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();
fstart_platform_intel::entry!(main);
fn main() -> ! {
    let rs = Ramstage::<Board>::enter();
    let mut pins = mainboard::Pins::of(rs.southbridge().gpio());
    mainboard::setup_dock_console();
    let resources: alloc::vec::Vec<_> =
        fstart_driver_lenovo::t6::firmware_resources(&fstart_board_lenovo_t61::T61_H8).collect();
    let mut rs = rs
        .console(&CONSOLE, mainboard::console())
        .memory_map()
        .scan_bus(&resources);
    rs.southbridge_mut()
        .set_ide_primary_enabled(mainboard::ultrabay_populated());
    let mut rs = rs.init_chipset();
    let resume = rs.is_resume();
    let ec = rs.resolve_set(&mainboard::EC, |_| Ok(()));
    rs.resolve_set(&mainboard::CPU, |_| Ok(())).apply();
    let identity = mainboard::init_devices(&mut pins.bluetooth_card, rs.smbus(), &ec, resume);
    let rs = rs
        .start_cpus()
        .verify_boot_media()
        .init_display()
        .init_pci_display(0x10de, fstart_board_lenovo_t61::acpi::DGPU);
    let rs = match rs.boot_path() {
        Path::Cold(rs) => rs.emit_tables(&identity.smbios(), &mainboard::Acpi),
        Path::Resume(rs) => rs.keep_tables(),
    };
    mainboard::handoff_ec_events(resume);
    rs.boot()
}
