//! ICH9/PCH software-sequencing SPI opcode menu. Preserves any pre-existing
//! flash configuration lockdown (`HSFS.FLOCKDN`).

use fstart_core::mmio::MmioWindow;

/// Program the standard ICH9/PCH SPI software-sequencing opcode menu through
/// the 16 KiB RCBA MMIO window. Returns `false` if `HSFS.FLOCKDN` is already
/// set.
pub fn spi_menu(rcba: MmioWindow) -> bool {
    const HSFS: u32 = 0x3804;
    const PREOP: u32 = 0x3894;
    const OPTYPE: u32 = 0x3896;
    const OPMENU: u32 = 0x3898;
    const FLOCKDN: u16 = 1 << 15;
    // (opcode, type): 0 read/no address, 1 write/no address,
    // 2 read/address, 3 write/address.
    const MENU: [(u8, u16); 8] = [
        (0x01, 1), // WRSR
        (0x02, 3), // byte program
        (0x03, 2), // read
        (0x05, 0), // RDSR
        (0x20, 3), // sector erase
        (0x9f, 0), // RDID
        (0xd8, 3), // block erase
        (0x0b, 2), // fast read
    ];
    if rcba.read16(HSFS) & FLOCKDN != 0 {
        return false;
    }
    // Prefixes: EWSR (0x50) and WREN (0x06).
    rcba.write16(PREOP, 0x5006);
    let mut optype = 0u16;
    for (index, (opcode, kind)) in MENU.iter().enumerate() {
        optype |= kind << (2 * index);
        rcba.write8(OPMENU + index as u32, *opcode);
    }
    rcba.write16(OPTYPE, optype);
    true
}
