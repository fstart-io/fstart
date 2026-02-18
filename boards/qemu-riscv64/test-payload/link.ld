/* Linker script for S-mode test payload.
 * Loaded at 0x80200000 by fstart via FFS → RustSBI jumps here in S-mode.
 * RustSBI passes: a0 = hartid, a1 = dtb_addr.
 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS {
    . = 0x80200000;

    .text : {
        *(.text.entry)
        *(.text .text.*)
    }

    .rodata : ALIGN(8) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(8) {
        *(.data .data.*)
    }

    .bss : ALIGN(8) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    /* Stack: 4 KiB, grows downward */
    . = ALIGN(16);
    . = . + 0x1000;
    __stack_top = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note.*)
    }
}
