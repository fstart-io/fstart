#pragma once
#include <stdint.h>
uint32_t fstart_vbios_io_read(uint16_t port, unsigned width);
void fstart_vbios_io_write(uint16_t port, unsigned width, uint32_t value);
static inline uint8_t inb(uint16_t p) { return fstart_vbios_io_read(p, 1); }
static inline uint16_t inw(uint16_t p) { return fstart_vbios_io_read(p, 2); }
static inline uint32_t inl(uint16_t p) { return fstart_vbios_io_read(p, 4); }
static inline void outb(uint8_t v, uint16_t p) { fstart_vbios_io_write(p, 1, v); }
static inline void outw(uint16_t v, uint16_t p) { fstart_vbios_io_write(p, 2, v); }
static inline void outl(uint32_t v, uint16_t p) { fstart_vbios_io_write(p, 4, v); }
