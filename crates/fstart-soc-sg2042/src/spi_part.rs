//! Sophgo gen_spi_flash disk partition table (DPT) parser.
//!
//! Reads the partition table from the SPI flash DMMR memory-mapped window
//! at flash offset `0x600000`. Used by the RISC-V release driver to locate
//! the ZSBL partition for loading into DDR.
//!
//! Hardware reference: `mango_misc.c` — `struct part_info`, `DPT_MAGIC`,
//! `DISK_PART_TABLE_ADDR`.
