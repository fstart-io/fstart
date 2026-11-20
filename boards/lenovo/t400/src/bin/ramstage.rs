//! T400-series ramstage: chipset, devices, hybrid graphics panel mux, tables
//! and the payload, or the OS wake vector on S3 resume.

#![no_std]
#![no_main]

use fstart_board_lenovo_t400::{Board, config, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::ramstage::{Path, Ramstage};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let rs = Ramstage::<Board>::enter();
    let mut pins = mainboard::Pins::of(rs.southbridge().gpio());
    let rs = rs
        .console(&CONSOLE, mainboard::console())
        .memory_map()
        .scan_bus(&[]);
    let mut rs = rs.init_chipset();
    let resume = rs.is_resume();
    let ec = rs.resolve_set(&mainboard::EC, |_| Ok(()));
    let hg = (!config::IS_R500).then(|| rs.resolve_set(&mainboard::HYBRID_GRAPHICS, |_| Ok(())));
    let (identity, acpi) = mainboard::init_devices(&mut pins, rs.smbus(), &ec, hg.as_ref(), resume);
    let rs = rs.start_cpus().verify_boot_media().init_display();
    let rs = match rs.boot_path() {
        Path::Cold(rs) => rs.emit_tables(&identity.smbios(), &acpi),
        Path::Resume(rs) => rs.keep_tables(),
    };
    rs.boot()
}
