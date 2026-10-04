/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/* PSP trains DRAM and copies the BIOS reset image to 0x02030000 before
 * releasing x86. ALL executable VMAs below are DRAM, NOT the SPI window.
 * Matches Picasso Kconfig: bootblock end=ROMSTAGE_ADDR=0x02050000.
 * Keep the complete reset image immutable for PSP S3 integrity checks.
 * This is a cold-boot console checkpoint, not an implemented S3 path. */
OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)
PHDRS { image PT_LOAD FLAGS(5); writable PT_LOAD FLAGS(6); mp PT_LOAD FLAGS(6); microcode PT_LOAD FLAGS(6); }
MEMORY {
    image (rx) : ORIGIN = 0x02030000, LENGTH = 0x20000
    writable (rw) : ORIGIN = 0x02019000, LENGTH = 0x17000
    mp (rw) : ORIGIN = 0x02050000, LENGTH = 0x11000
    microcode (rw) : ORIGIN = 0x02061000, LENGTH = 0x1000
}
_fstart_apob_start = 0x02001000;
_fstart_apob_end = 0x02011000;
_fstart_psp_shared_start = 0x02011000;
_fstart_psp_shared_end = 0x02019000;
_fstart_early_reserved_start = 0x02000000;
_fstart_early_reserved_end = 0x02062000;
_fstart_page_tables = 0x02020000;
_fstart_page_tables_end = 0x02026000;
_fstart_stack_bottom = 0x02026000;
_fstart_stack_top = 0x02030000;
SECTIONS {
    .text 0x02030000 : { *(.text .text.*) } > image :image
    .rodata : { *(.rodata .rodata.*) *(.srodata .srodata.*) KEEP(*(.picasso_microcode_input)) } > image :image
    /* LLD's static GOT has SHF_WRITE but is host-resolved pointer metadata:
     * no runtime loader/patching. Put it in the immutable RX LOAD, not BSS.
     * This is ELF placement, NOT runtime page write-protection. */
    .static_got : { *(.got) } > image :image
    /* Keep unlike ELF section types separate, even when LLD synthesizes
     * empty sections. No dynamic loader or runtime relocations are supported. */
    .unsupported_dynamic : { *(.dynamic) } > image :image
    .unsupported_rel : { *(.rel.*) } > image :image
    .unsupported_rela : { *(.rela.*) } > image :image
    .unsupported_relr : { *(.relr.*) } > image :image
    .unsupported_loader : { *(.interp .plt .plt.* .iplt) } > image :image
    . = ALIGN(16);
    _fstart_text_end = .;
    .init16 0x0204e000 : { KEEP(*(.init16)) } > image :image
    .reset 0x0204fff0 : { KEEP(*(.reset)) } > image :image
    .last_byte 0x0204ffff : { BYTE(0xff); } > image :image
    .data 0x02019000 : { *(.data .data.* .sdata .sdata.*) *(.got.*) } > writable :writable
    .bss (NOLOAD) : {
        _fstart_bss_start = .;
        *(.bss .bss.* .sbss .sbss.*) *(COMMON)
        . = ALIGN(16);
        _fstart_bss_end = .;
    } > writable :writable
    .picasso_mp ORIGIN(mp) (NOLOAD) : {
        _fstart_mp_start = .;
        KEEP(*(.picasso_mp))
        _fstart_mp_allocation_end = .;
        . = ALIGN(0x1000);
        _fstart_mp_end = .;
    } > mp :mp
    .picasso_microcode ORIGIN(microcode) (NOLOAD) : {
        _fstart_microcode_start = .;
        KEEP(*(.picasso_microcode))
        _fstart_microcode_end = .;
    } > microcode :microcode
    /DISCARD/ : { *(.eh_frame*) *(.comment*) *(.note*) }
}
_fstart_gdt_offset = _fstart_gdt_pointer & 0xffff;
ASSERT(_fstart_early_reserved_end == _fstart_microcode_end, "early firmware reservation must include resident microcode memory")
ASSERT(_fstart_microcode_start == 0x02061000 && _fstart_microcode_end == 0x02062000, "microcode layout must match platform reservation")
ASSERT(_fstart_mp_end == _fstart_microcode_start, "resident regions must not overlap")
ASSERT(SIZEOF(.picasso_microcode) == 0x1000, "microcode storage capacity/layout mismatch")
ASSERT(_fstart_mp_start == 0x02050000 && _fstart_mp_end == 0x02061000, "MP layout must match platform reservation")
ASSERT(_fstart_mp_allocation_end - _fstart_mp_start == 0x10800, "MP storage capacity/layout mismatch")
ASSERT(SIZEOF(.data) == 0, "checkpoint must not mutate the PSP reset image: initialized data unsupported")
ASSERT(SIZEOF(.unsupported_dynamic) + SIZEOF(.unsupported_rel) + SIZEOF(.unsupported_rela) + SIZEOF(.unsupported_relr) + SIZEOF(.unsupported_loader) == 0, "Picasso image cannot require runtime linking or IFUNC resolution")
ASSERT(_fstart_text_end <= 0x0204e000, "Picasso reset code exceeds DRAM image budget")
ASSERT(ADDR(.init16) + SIZEOF(.init16) <= ADDR(.reset), "16-bit entry overflows reset vector")
ASSERT(SIZEOF(.reset) <= 15, "reset vector too large")
ASSERT(_fstart_bss_end <= _fstart_page_tables, "BSS overlaps page tables")
ASSERT(_fstart_stack_bottom >= _fstart_page_tables_end, "stack overlaps page tables")
ASSERT(_fstart_stack_top == ORIGIN(image), "stack overlaps immutable reset image")
ASSERT(_fstart_psp_shared_end <= ORIGIN(writable), "writable memory overlaps PSP transfer buffer")
ASSERT(_fstart_apob_end <= _fstart_psp_shared_start, "PSP reservations overlap")
ASSERT((ORIGIN(image) + LENGTH(image)) % 0x10000 == 0, "PSP reset image end must be 64-KiB aligned")
