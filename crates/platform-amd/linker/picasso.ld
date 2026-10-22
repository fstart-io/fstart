/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/* The PSP trains DRAM and copies this image to IMAGE_BASE before releasing
 * x86 at its last 16 bytes. All VMAs are DRAM, not the SPI window.
 * Keep in sync with src/layout.rs. There is no initialized writable data, so
 * the PSP-copied image is never written (the PSP checks it on S3 resume). */
OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)
PHDRS { image PT_LOAD FLAGS(5); writable PT_LOAD FLAGS(6); microcode PT_LOAD FLAGS(6); psp PT_LOAD FLAGS(6); }
MEMORY {
    image (rx) : ORIGIN = 0x02100000, LENGTH = 0x200000
    writable (rw) : ORIGIN = 0x02020000, LENGTH = 0xb0000
    microcode (rw) : ORIGIN = 0x02019000, LENGTH = 0x1000
    psp (rw) : ORIGIN = 0x0201a000, LENGTH = 0x1000
}
_fstart_apob_start = 0x02001000;
_fstart_apob_end = 0x02011000;
_fstart_psp_shared_start = 0x02011000;
_fstart_psp_shared_end = 0x02019000;
_fstart_early_reserved_start = 0x02000000;
_fstart_early_reserved_end = 0x02300000;
_fstart_page_tables = 0x020d0000;
_fstart_page_tables_end = 0x020d6000;
_fstart_stack_bottom = 0x020e0000;
_fstart_stack_top = 0x02100000;
SECTIONS {
    .text 0x02100000 : { *(.text .text.*) } > image :image
    .rodata : { *(.rodata .rodata.*) *(.srodata .srodata.*) } > image :image
    /* LLD's static GOT holds link-time-resolved pointers: keep it in the
     * read-only image. No runtime loader or relocations are supported. */
    .static_got : { *(.got) } > image :image
    .unsupported_dynamic : { *(.dynamic) } > image :image
    .unsupported_rel : { *(.rel.*) } > image :image
    .unsupported_rela : { *(.rela.*) } > image :image
    .unsupported_relr : { *(.relr.*) } > image :image
    .unsupported_loader : { *(.interp .plt .plt.* .iplt) } > image :image
    . = ALIGN(16);
    _fstart_text_end = .;
    .init16 0x022fe000 : { KEEP(*(.init16)) } > image :image
    .reset 0x022ffff0 : { KEEP(*(.reset)) } > image :image
    .last_byte 0x022fffff : { BYTE(0xff); } > image :image
    .data 0x02020000 : { *(.data .data.* .sdata .sdata.*) *(.got.*) } > writable :writable
    .bss (NOLOAD) : {
        _fstart_bss_start = .;
        *(.bss .bss.* .sbss .sbss.*) *(COMMON)
        . = ALIGN(16);
        _fstart_bss_end = .;
    } > writable :writable
    .picasso_microcode ORIGIN(microcode) (NOLOAD) : {
        _fstart_microcode_start = .;
        KEEP(*(.picasso_microcode))
        _fstart_microcode_end = .;
    } > microcode :microcode
    .picasso_psp ORIGIN(psp) (NOLOAD) : {
        _fstart_psp_start = .;
        KEEP(*(.picasso_psp))
        _fstart_psp_end = .;
    } > psp :psp
    /DISCARD/ : { *(.eh_frame*) *(.comment*) *(.note*) }
}
_fstart_gdt_offset = _fstart_gdt_pointer & 0xffff;
ASSERT(SIZEOF(.picasso_psp) == 0x1000, "PSP mailbox storage capacity/layout mismatch")
ASSERT(SIZEOF(.picasso_microcode) == 0x1000, "microcode storage capacity/layout mismatch")
ASSERT(_fstart_microcode_start >= _fstart_psp_shared_end, "microcode page overlaps PSP shared memory")
ASSERT(SIZEOF(.unsupported_dynamic) + SIZEOF(.unsupported_rel) + SIZEOF(.unsupported_rela) + SIZEOF(.unsupported_relr) + SIZEOF(.unsupported_loader) == 0, "Picasso image cannot require runtime linking or IFUNC resolution")
_data_start = ADDR(.data);
_data_end = ADDR(.data) + SIZEOF(.data);
_data_load = LOADADDR(.data);
ASSERT(SIZEOF(.data) == 0, "initialized writable data would modify the PSP-copied image")
ASSERT(_fstart_text_end <= ADDR(.init16), "Picasso code exceeds the DRAM image")
ASSERT(ADDR(.init16) + SIZEOF(.init16) <= ADDR(.reset), "16-bit entry overflows reset vector")
ASSERT(SIZEOF(.reset) <= 15, "reset vector too large")
ASSERT(_fstart_bss_end <= _fstart_page_tables, "BSS overlaps page tables")
ASSERT(_fstart_stack_bottom >= _fstart_page_tables_end, "stack overlaps page tables")
ASSERT(_fstart_stack_top <= ORIGIN(image), "stack overlaps the PSP-copied image")
ASSERT(_fstart_apob_end <= _fstart_psp_shared_start, "PSP reservations overlap")
ASSERT(ORIGIN(image) + LENGTH(image) == _fstart_early_reserved_end, "early reservation must cover the image")
ASSERT((ORIGIN(image) + LENGTH(image)) % 0x10000 == 0, "PSP reset image end must be 64-KiB aligned")
