//! Wire-format and malformed-input checks with a byte-stream fw_cfg transport.

extern crate std;

use super::*;
use std::cell::Cell;
use std::vec::Vec;

#[derive(Clone, Copy)]
struct Mock<'data, 'state> {
    dir: &'data [u8],
    e820: &'data [u8],
    loader: &'data [u8],
    blob: &'data [u8],
    selector: &'state Cell<u16>,
    position: &'state Cell<usize>,
}

impl FwCfgTransport for Mock<'_, '_> {
    fn select(self, selector: u16) {
        self.selector.set(selector);
        self.position.set(0);
    }

    fn read_byte(self) -> u8 {
        let stream = match self.selector.get() {
            FW_CFG_FILE_DIR => self.dir,
            0x20 => self.e820,
            0x21 => self.loader,
            0x22 => self.blob,
            _ => panic!("unknown test selector"),
        };
        let pos = self.position.get();
        self.position.set(pos + 1);
        stream[pos]
    }
}

fn directory(files: &[(&str, u32, u16)]) -> Vec<u8> {
    let mut dir = Vec::new();
    dir.extend_from_slice(&(files.len() as u32).to_be_bytes());
    for (name, size, selector) in files {
        dir.extend_from_slice(&size.to_be_bytes());
        dir.extend_from_slice(&selector.to_be_bytes());
        dir.extend_from_slice(&0u16.to_be_bytes());
        let mut name_bytes = [0; 56];
        name_bytes[..name.len()].copy_from_slice(name.as_bytes());
        dir.extend_from_slice(&name_bytes);
    }
    dir
}

fn run<'a>(
    dir: &'a [u8],
    e820: &'a [u8],
    loader: &'a [u8],
    blob: &'a [u8],
    f: impl FnOnce(QemuFwCfg<Mock<'a, '_>>),
) {
    let selector = Cell::new(0);
    let position = Cell::new(0);
    f(QemuFwCfg::new(Mock {
        dir,
        e820,
        loader,
        blob,
        selector: &selector,
        position: &position,
    }));
}

#[test]
fn e820_rejects_partial_records_and_full_buffers() {
    let mut bytes = [0u8; 40];
    bytes[..8].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&4096u64.to_le_bytes());
    bytes[16..20].copy_from_slice(&(E820Kind::Ram as u32).to_le_bytes());
    let dir = directory(&[("etc/e820", 20, 0x20)]);
    run(&dir, &bytes, &[], &[], |cfg| {
        let mut entries = [E820Entry::zeroed(); 1];
        assert_eq!(cfg.detect_memory(&mut entries), Ok(1));
        let addr = entries[0].addr;
        let size = entries[0].size;
        assert_eq!(addr, 0x1122_3344_5566_7788);
        assert_eq!(size, 4096);
        assert_eq!(cfg.total_ram_bytes(), Ok(4096));
    });
    for size in [21, 40] {
        let dir = directory(&[("etc/e820", size, 0x20)]);
        run(&dir, &bytes, &[], &[], |cfg| {
            let mut entries = [E820Entry::zeroed(); 1];
            assert_eq!(
                cfg.detect_memory(&mut entries),
                Err(ServiceError::InvalidParam)
            );
            if size == 21 {
                assert_eq!(cfg.total_ram_bytes(), Err(ServiceError::InvalidParam));
            }
        });
    }
}

#[test]
fn table_loader_rejects_misalignment_and_out_of_file_patches() {
    let mut alloc = [0u8; LOADER_COMMAND_SIZE];
    alloc[..4].copy_from_slice(&COMMAND_ALLOCATE.to_le_bytes());
    alloc[4..8].copy_from_slice(b"blob");
    alloc[60..64].copy_from_slice(&1u32.to_le_bytes());
    let mut patch = [0u8; LOADER_COMMAND_SIZE];
    patch[..4].copy_from_slice(&COMMAND_ADD_POINTER.to_le_bytes());
    patch[4..8].copy_from_slice(b"blob");
    patch[60..64].copy_from_slice(b"blob");
    patch[116..120].copy_from_slice(&12u32.to_le_bytes());
    patch[120] = 8; // ends beyond the 16-byte blob

    let blob = [0u8; 16];
    let mut buffer = [0u8; 128];
    let pointer_commands = [alloc.as_slice(), patch.as_slice()].concat();
    for commands in [&alloc[..127], pointer_commands.as_slice()] {
        let dir = directory(&[
            ("etc/table-loader", commands.len() as u32, 0x21),
            ("blob", 16, 0x22),
        ]);
        run(&dir, &[], commands, &blob, |cfg| {
            assert_eq!(
                cfg.load_acpi_tables(&mut buffer),
                Err(ServiceError::InvalidParam)
            );
        });
    }
    alloc[60..64].copy_from_slice(&3u32.to_le_bytes());
    let dir = directory(&[("etc/table-loader", 128, 0x21), ("blob", 16, 0x22)]);
    run(&dir, &[], &alloc, &blob, |cfg| {
        assert_eq!(
            cfg.load_acpi_tables(&mut buffer),
            Err(ServiceError::InvalidParam)
        );
    });

    alloc[60..64].copy_from_slice(&1u32.to_le_bytes());
    patch[..4].copy_from_slice(&COMMAND_ADD_CHECKSUM.to_le_bytes());
    patch[60..64].copy_from_slice(&16u32.to_le_bytes()); // checksum byte beyond blob
    let commands = [alloc.as_slice(), patch.as_slice()].concat();
    let dir = directory(&[("etc/table-loader", 256, 0x21), ("blob", 16, 0x22)]);
    run(&dir, &[], &commands, &blob, |cfg| {
        assert_eq!(
            cfg.load_acpi_tables(&mut buffer),
            Err(ServiceError::InvalidParam)
        );
    });

    // QEMU pads the loader file with zero commands; unknown nonzero opcodes
    // are genuinely unsupported rather than silently ignored.
    let mut command = [0u8; LOADER_COMMAND_SIZE];
    let dir = directory(&[("etc/table-loader", 128, 0x21)]);
    run(&dir, &[], &command, &[], |cfg| {
        assert_eq!(
            cfg.load_acpi_tables(&mut buffer),
            Err(ServiceError::IoError)
        );
    });
    command[..4].copy_from_slice(&5u32.to_le_bytes());
    run(&dir, &[], &command, &[], |cfg| {
        assert_eq!(
            cfg.load_acpi_tables(&mut buffer),
            Err(ServiceError::NotSupported)
        );
    });
}

#[test]
fn write_pointer_does_not_block_acpi_loading_without_dma_write_support() {
    let mut loader = [0u8; 2 * LOADER_COMMAND_SIZE];
    loader[..4].copy_from_slice(&COMMAND_ALLOCATE.to_le_bytes());
    loader[4..17].copy_from_slice(b"etc/acpi/rsdp");
    loader[60..64].copy_from_slice(&1u32.to_le_bytes());
    loader[LOADER_COMMAND_SIZE..LOADER_COMMAND_SIZE + 4]
        .copy_from_slice(&COMMAND_WRITE_POINTER.to_le_bytes());
    // vmgenid_addr is a writable fw_cfg file, not a guest ACPI allocation.
    loader[LOADER_COMMAND_SIZE + 4..LOADER_COMMAND_SIZE + 20].copy_from_slice(b"etc/vmgenid_addr");
    let dir = directory(&[
        ("etc/table-loader", loader.len() as u32, 0x21),
        ("etc/acpi/rsdp", 36, 0x22),
    ]);
    let mut rsdp = [0u8; 36];
    rsdp[..8].copy_from_slice(b"RSD PTR ");
    let mut buffer = [0u8; 128];
    let expected = buffer.as_ptr() as u64;
    run(&dir, &[], &loader, &rsdp, |cfg| {
        assert_eq!(cfg.load_acpi_tables(&mut buffer), Ok(expected));
    });
}
