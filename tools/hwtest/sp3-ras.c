// SPDX-License-Identifier: MIT OR Apache-2.0
// Linux/initramfs-only SP3 checks; not for production hosts. No error injection
// or disk writes. The optional SEL probe adds one benign auxiliary-log event.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/io.h>
#include <time.h>
#include <unistd.h>

static unsigned failures;
static uint64_t rd(int fd, uint32_t reg)
{
    uint64_t value;
    if (pread(fd, &value, sizeof(value), reg) != sizeof(value)) {
        fprintf(stderr, "RAS FAIL read %08x: %m\n", reg);
        exit(EXIT_FAILURE); // Never treat an unsuccessful status read as zero.
    }
    return value;
}
static int wr(int fd, uint32_t reg, uint64_t value)
{
    if (pwrite(fd, &value, sizeof(value), reg) == sizeof(value)) return 0;
    fprintf(stderr, "RAS FAIL write %08x: %m\n", reg);
    ++failures;
    return -1;
}
static int pin(unsigned cpu)
{
    cpu_set_t cpus;
    CPU_ZERO(&cpus); CPU_SET(cpu, &cpus);
    if (!sched_setaffinity(0, sizeof(cpus), &cpus)) return 0;
    perror("RAS FAIL affinity"); ++failures;
    return -1;
}
struct diagnostic { uint64_t a, c, d; };
static struct diagnostic diag(uint64_t selector)
{
    // Leave time for the FCH's previous SMI edge to deassert. The request
    // is diagnostic traffic, not a stress test of back-to-back SMI delivery.
    usleep(1000);
    struct diagnostic r = {0x46535241530000d1ULL, selector, 0xb2};
    // Explicit register outputs: only the caller carrying this full request
    // token is modified. No shared memory addresses are passed into SMM.
    __asm__ volatile("outb %%al, %%dx" : "+a"(r.a), "+c"(r.c), "+d"(r.d) : : "memory");
    return r;
}
static int check_cpu(unsigned cpu)
{
    char path[64];
    snprintf(path, sizeof(path), "/dev/cpu/%u/msr", cpu);
    int fd = open(path, O_RDWR);
    if (fd < 0) {
        if (errno != ENOENT && errno != ENXIO) { perror(path); ++failures; }
        return 0;
    }
    if (pin(cpu)) { close(fd); return 1; }
    uint64_t cap = rd(fd, 0x179), redir = rd(fd, 0xc0010022);
    uint64_t trigger = rd(fd, 0xc0010056), outside = rd(fd, 0xc0010120);
    struct diagnostic smm = diag(0);
    int armed = (redir & 0x300) == 0x200 && (trigger & 0x06ffffff) == 0x028000b2
        && (smm.a & 0xf) == 1 && smm.c == 0 && smm.d == 0;
    printf("RAS cpu=%u banks=%" PRIu64 " redir=%016" PRIx64
           " trigger=%016" PRIx64 " pfeh_normal=%016" PRIx64
           " pfeh_smm=%016" PRIx64 " cloak_smm=%016" PRIx64
           " mask_smm=%016" PRIx64 " %s\n", cpu, cap & 255,
           redir, trigger, outside, smm.a, smm.c, smm.d, armed ? "ARMED" : "NOT_ARMED");
    if (!armed) ++failures;
    close(fd);
    return 1;
}
int main(void)
{
    puts("RAS_HWTEST BEGIN: registers/SMI/KCS probe, no injected machine check");
    if (ioperm(0xb2, 1, 1)) { perror("RAS FAIL ioperm"); return EXIT_FAILURE; }
    unsigned present = 0;
    for (unsigned cpu = 0; cpu < 256; ++cpu) present += check_cpu(cpu);
    if (!present) ++failures;
    if (failures || pin(0)) {
        printf("RAS_HWTEST END cpus=%u failures=%u (no write tests attempted)\n", present, failures);
        return EXIT_FAILURE;
    }
    struct diagnostic layout = diag(3);
    printf("RAS SAVE_STATE FF00_match=%" PRIu64 " FE00_match=%" PRIu64 " revision=%" PRIx64 "\n", layout.a, layout.c, layout.d);
    struct timespec start, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    struct diagnostic result = diag(1);
    for (unsigned i = 0; i < 4 && (result.a == 1 || result.a == 4); ++i) result = diag(2);
    clock_gettime(CLOCK_MONOTONIC, &end);
    uint64_t us = (end.tv_sec - start.tv_sec) * 1000000ULL + (end.tv_nsec - start.tv_nsec) / 1000;
    printf("RAS SEL_PROBE result=%" PRIu64 " elapsed_us=%" PRIu64 " (2=success,3=failure)\n", result.a, us);
    if (result.a != 2) ++failures;
    unsigned before = failures;
    // A software command alone must not manufacture a local MCA cause.
    usleep(1000);
    outb(0x80, 0xb2);
    puts("RAS SW_SMI_80 RETURNED (not an MCA injection)");
    check_cpu(0);
    if (failures != before) goto finish;
    int fd = open("/dev/cpu/0/msr", O_RDWR);
    if (fd >= 0) {
        uint64_t status = rd(fd, 0xc0002001);
        if (status == 0) {
            if (!wr(fd, 0xc0002001, 0)) {
                puts("RAS ZERO_STATUS_CLEAR RETURNED");
                if (rd(fd, 0xc0002001) != 0) ++failures;
            }
        } else {
            printf("RAS SKIP clear: existing status=%016" PRIx64 "\n", status);
            ++failures;
        }
        close(fd);
    } else { perror("RAS FAIL CPU0 msr"); ++failures; }
    check_cpu(0);
finish:
    ioperm(0xb2, 1, 0);
    printf("RAS_HWTEST END cpus=%u failures=%u\n", present, failures);
    return failures ? EXIT_FAILURE : EXIT_SUCCESS;
}
