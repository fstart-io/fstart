/* SPDX-License-Identifier: MIT */
/* Keep the emulator's packed register unions private to C. */
#include <x86emu/x86emu.h>
#include "prim_ops.h"

struct fstart_vbios_regs {
    uint32_t eax, ebx, ecx, edx, esi, edi;
    uint16_t es, flags;
};
extern uint32_t fstart_vbios_mem_read(uint32_t address, unsigned width);
extern void fstart_vbios_mem_write(uint32_t address, unsigned width, uint32_t value);
extern int fstart_vbios_interrupt(unsigned number, struct fstart_vbios_regs *regs);
extern void fstart_vbios_fault(uint16_t cs, uint16_t ip);
void x86emu_dump_regs(void) { fstart_vbios_fault(M.x86.R_CS, M.x86.R_IP); }
uint32_t fstart_vbios_position(void) { return ((uint32_t)M.x86.R_CS << 16) | M.x86.R_IP; }
static uint8_t read8(uint32_t a) { return fstart_vbios_mem_read(a, 1); }
static uint16_t read16(uint32_t a) { return fstart_vbios_mem_read(a, 2); }
static uint32_t read32(uint32_t a) { return fstart_vbios_mem_read(a, 4); }
static void write8(uint32_t a, uint8_t v) { fstart_vbios_mem_write(a, 1, v); }
static void write16(uint32_t a, uint16_t v) { fstart_vbios_mem_write(a, 2, v); }
static void write32(uint32_t a, uint32_t v) { fstart_vbios_mem_write(a, 4, v); }
static void get_regs(struct fstart_vbios_regs *r) {
    *r = (struct fstart_vbios_regs) { M.x86.R_EAX, M.x86.R_EBX, M.x86.R_ECX,
        M.x86.R_EDX, M.x86.R_ESI, M.x86.R_EDI, M.x86.R_ES, M.x86.R_FLG };
}
static void set_regs(const struct fstart_vbios_regs *r) {
    M.x86.R_EAX=r->eax; M.x86.R_EBX=r->ebx; M.x86.R_ECX=r->ecx;
    M.x86.R_EDX=r->edx; M.x86.R_ESI=r->esi; M.x86.R_EDI=r->edi;
    M.x86.R_ES=r->es; M.x86.R_FLG=r->flags;
}
static void interrupt(int number) {
    const uint32_t vector = read32(number * 4);
    const uint32_t target = ((vector >> 16) << 4) + (vector & 0xffff);
    if (target >= 0xc0000 && target < 0xe0000) {
        X86EMU_prepareForInt(number);
        return;
    }
    struct fstart_vbios_regs regs;
    get_regs(&regs);
    if (!fstart_vbios_interrupt(number, &regs)) {
        X86EMU_halt_sys();
        return;
    }
    set_regs(&regs);
}
static void stack(void) {
    M.x86.R_SS = 0x7000; M.x86.R_SP = 0xfffe;
    M.x86.R_CS = 0x1000; M.x86.R_EIP = 0;
    M.x86.intr = 0;
}
static int finished(void) {
    return M.x86.R_CS == 0x1000 && M.x86.R_IP == 1;
}
int fstart_vbios_post(void *memory, size_t size, unsigned bdf) {
    memset(&M, 0, sizeof(M));
    X86EMU_setMemBase(memory, size);
    X86EMU_memFuncs mem = { read8, read16, read32, write8, write16, write32 };
    X86EMU_setupMemFuncs(&mem);
    X86EMU_intrFuncs intr[256];
    for (unsigned i = 0; i < 256; ++i) intr[i] = interrupt;
    X86EMU_setupIntrFuncs(intr);
    stack();
    M.x86.R_EAX = bdf; M.x86.R_EDX = 0x80; M.x86.R_FLG = 2;
    push_word(0x1000); push_word(0);
    M.x86.R_CS = 0xc000; M.x86.R_EIP = 3;
    X86EMU_exec();
    return finished();
}
int fstart_vbios_int10(struct fstart_vbios_regs *regs) {
    stack();
    set_regs(regs);
    X86EMU_prepareForInt(0x10);
    X86EMU_exec();
    get_regs(regs);
    return finished();
}
