#pragma once
#define BIOS_DEBUG 0
static inline int printk(int level, const char *format, ...) { (void)level; (void)format; return 0; }
