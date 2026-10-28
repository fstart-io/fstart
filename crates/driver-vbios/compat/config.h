/* The emulator's coreboot-only build switches; tracing is handled by Rust. */
#ifndef CONFIG
#define CONFIG(option) 0
#endif
#define __printf(a, b) __attribute__((format(printf, a, b)))
#include <stddef.h>
#include <stdint.h>
typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;
typedef int8_t s8;
typedef int16_t s16;
typedef int32_t s32;
typedef int64_t s64;
void *memset(void *, int, size_t);
