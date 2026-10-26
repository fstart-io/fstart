/* Disposable initramfs only: standard Linux EINJ, one reserved scratch page.
 * No host block devices, arbitrary physical addresses, or fatal injection.
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <unistd.h>

#define BASE 0x75bf0000ULL
#define SCRATCH (BASE + 4096)
#define EINJ "/sys/kernel/debug/apei/einj/"

static int put(const char *name, const char *value)
{
    char path[128];
    snprintf(path, sizeof(path), EINJ "%s", name);
    int fd = open(path, O_WRONLY);
    if (fd < 0) { perror(path); return -1; }
    usleep(1000);
    ssize_t n = write(fd, value, strlen(value));
    int error = errno;
    close(fd);
    errno = error;
    return n == (ssize_t)strlen(value) ? 0 : -1;
}

static long ce_count(void)
{
    FILE *f = fopen("/sys/devices/system/edac/mc/mc0/ce_count", "r");
    long count = -1;
    if (f) { if (fscanf(f, "%ld", &count) != 1) count = -1; fclose(f); }
    return count;
}

static void backend(void)
{
    int fd = open("/dev/mem", O_RDONLY | O_SYNC);
    if (fd < 0) { perror("EINJ backend /dev/mem"); return; }
    volatile uint64_t *p = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, BASE);
    if (p == MAP_FAILED) { perror("EINJ backend mmap"); close(fd); return; }
    printf("EINJ backend supported=%" PRIx64 " busy=%" PRIu64 " status=%" PRIu64
           " enable=%" PRIx64 " begin=%" PRIx64 " setup=%" PRIx64 " end=%" PRIx64 "\n",
           p[0], p[2], p[3], p[24], p[25], p[26], p[27]);
    printf("EINJ registration=%" PRIx64 " policy=%" PRIu64 " trace_count=%" PRIu64 "\n", p[28], p[30], p[29]);
    uint64_t count = p[29];
    if (count > 32) count = 32;
    for (uint64_t i = 0; i < count; ++i) {
        uint64_t pair = p[32 + i];
        printf("EINJ target-read tag=%08" PRIx32 " value=%08" PRIx32 "\n", (uint32_t)pair, (uint32_t)(pair >> 32));
    }
    munmap((void *)p, 4096);
    close(fd);
}

int main(void)
{
    setbuf(stdout, NULL);
    puts("EINJ_HWTEST BEGIN (correctable DRAM only)");
    mount("debugfs", "/sys/kernel/debug", "debugfs", 0, NULL);
    FILE *f = fopen(EINJ "available_error_type", "r");
    backend();
    if (!f) { perror("EINJ available_error_type"); return 1; }
    char line[256];
    unsigned types = 0;
    while (fgets(line, sizeof(line), f)) {
        unsigned type;
        printf("EINJ available %s", line);
        if (sscanf(line, "%x", &type) == 1) types |= type;
    }
    fclose(f);
    if (types != 8) { puts("EINJ unexpected supported types; refusing"); return 1; }
    long before = ce_count();
    if (before < 0) { puts("EINJ no EDAC baseline; refusing"); return 1; }
    printf("EINJ baseline ce=%ld target=0x%llx\n", before, SCRATCH);
    if (put("flags", "2") || put("param1", "0x75bf1000") ||
        put("param2", "0xfffffffffffff000") || put("error_type", "8") ||
        put("error_inject", "1")) {
        perror("EINJ injection"); backend(); return 1;
    }
    puts("EINJ Linux execute/trigger/end returned success");
    backend();
    long after = before;
    for (int i = 0; i < 35 && after <= before; ++i) { sleep(1); after = ce_count(); }
    printf("EINJ result ce_before=%ld ce_after=%ld\n", before, after);
    if (after <= before) { puts("EINJ_HWTEST FAILED: no hardware corrected-error evidence"); return 1; }
    /* The firmware must reject even another reserved page: not scratch. */
    if (put("param1", "0x75bf0000")) { perror("EINJ invalid-target setup"); return 1; }
    if (!put("error_inject", "1") || errno != EINVAL) {
        puts("EINJ_HWTEST FAILED: non-scratch target not rejected with EINVAL"); backend(); return 1;
    }
    puts("EINJ non-scratch target rejected with EINVAL");
    backend();
    puts("EINJ_HWTEST PASS: real corrected-error report; no #MC handoff claim");
    return 0;
}
