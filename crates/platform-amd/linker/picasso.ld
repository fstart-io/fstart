/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/* PSP trains DRAM and copies the BIOS reset image to 0x02030000 before
 * releasing x86. ALL executable VMAs below are DRAM, NOT the SPI window.
 * Matches Picasso Kconfig: bootblock end=ROMSTAGE_ADDR=0x02050000.
 * Keep the complete reset image immutable for PSP S3 integrity checks.
 * This is a cold-boot console checkpoint, not an implemented S3 path. */
OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)
PHDRS { image PT_LOAD FLAGS(5); writable PT_LOAD FLAGS(6); }
MEMORY {
    image (rx) : ORIGIN = 0x02030000, LENGTH = 0x20000
    writable (rw) : ORIGIN = 0x02019000, LENGTH = 0x17000
}
_fstart_apob_start = 0x02001000;
_fstart_apob_end = 0x02011000;
_fstart_psp_shared_start = 0x02011000;
_fstart_psp_shared_end = 0x02019000;
_fstart_early_reserved_start = 0x02000000;
_fstart_early_reserved_end = 0x02050000;
_fstart_page_tables = 0x02020000;
_fstart_page_tables_end = 0x02026000;
_fstart_stack_bottom = 0x02026000;
_fstart_stack_top = 0x02030000;
SECTIONS {
    .text 0x02030000 : { *(.text .text.*) } > image :image
    .rodata : { *(.rodata .rodata.*) *(.srodata .srodata.*) } > image :image
    . = ALIGN(16);
    _fstart_text_end = .;
    .init16 0x0204e000 : { KEEP(*(.init16)) } > image :image
    .reset 0x0204fff0 : { KEEP(*(.reset)) } > image :image
    .last_byte 0x0204ffff : { BYTE(0xff); } > image :image
    .data 0x02019000 : { *(.data .data.* .sdata .sdata.*) *(.got .got.*) } > writable :writable
    .bss (NOLOAD) : {
        _fstart_bss_start = .;
        *(.bss .bss.* .sbss .sbss.*) *(COMMON)
        . = ALIGN(16);
        _fstart_bss_end = .;
    } > writable :writable
    /DISCARD/ : { *(.eh_frame*) *(.comment*) *(.note*) }
}
_fstart_gdt_offset = _fstart_gdt_pointer & 0xffff;
ASSERT(SIZEOF(.data) == 0, "checkpoint must not mutate the PSP reset image: initialized data unsupported")
ASSERT(_fstart_text_end <= 0x0204e000, "Picasso reset code exceeds DRAM image budget")
ASSERT(ADDR(.init16) + SIZEOF(.init16) <= ADDR(.reset), "16-bit entry overflows reset vector")
ASSERT(SIZEOF(.reset) <= 15, "reset vector too large")
ASSERT(_fstart_bss_end <= _fstart_page_tables, "BSS overlaps page tables")
ASSERT(_fstart_stack_bottom >= _fstart_page_tables_end, "stack overlaps page tables")
ASSERT(_fstart_stack_top == ORIGIN(image), "stack overlaps immutable reset image")
ASSERT(_fstart_psp_shared_end <= ORIGIN(writable), "writable memory overlaps PSP transfer buffer")
ASSERT(_fstart_apob_end <= _fstart_psp_shared_start, "PSP reservations overlap")
ASSERT((ORIGIN(image) + LENGTH(image)) % 0x10000 == 0, "PSP reset image end must be 64-KiB aligned")
