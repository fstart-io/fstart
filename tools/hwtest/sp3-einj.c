/* Disposable, authorized initramfs only. Native Linux EINJ on firmware-owned
 * per-socket scratch pages, never arbitrary addresses or fatal injection.
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <glob.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <unistd.h>

#define BASE 0x75bf0000ULL
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
    glob_t paths = {0};
    if (glob("/sys/devices/system/edac/mc/mc*/ce_count", 0, NULL, &paths)) return -1;
    long total = 0;
    for (size_t i = 0; i < paths.gl_pathc; ++i) {
        FILE *f = fopen(paths.gl_pathv[i], "r");
        long count;
        if (!f || fscanf(f, "%ld", &count) != 1) { if (f) fclose(f); globfree(&paths); return -1; }
        total += count;
        fclose(f);
    }
    globfree(&paths);
    return total;
}

static unsigned backend(uint64_t targets[2])
{
    int fd = open("/dev/mem", O_RDONLY | O_SYNC);
    if (fd < 0) { perror("EINJ backend /dev/mem"); return 0; }
    volatile uint64_t *p = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, BASE);
    if (p == MAP_FAILED) { perror("EINJ backend mmap"); close(fd); return 0; }
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
    uint64_t sockets = p[66];
    if (sockets > 2) sockets = 0;
    for (unsigned s = 0; s < sockets; ++s) {
        uint64_t target = p[64 + s];
        printf("EINJ socket=%u target=%" PRIx64 "\n", s, target);
        if (targets) targets[s] = target;
    }
    munmap((void *)p, 4096);
    close(fd);
    return (unsigned)sockets;
}

int main(void)
{
    setbuf(stdout, NULL);
    puts("EINJ_HWTEST BEGIN (correctable DRAM only)");
    /* Refuse an unrelated BIOS before reading the known communication page. */
    unsigned char header[16];
    FILE *f = fopen("/sys/firmware/acpi/tables/FACP", "rb");
    if (!f || fread(header, 1, sizeof(header), f) != sizeof(header) || memcmp(header + 10, "FSTART", 6)) {
        if (f) fclose(f);
        puts("EINJ not fstart firmware; refusing"); return 1;
    }
    fclose(f);
    mount("debugfs", "/sys/kernel/debug", "debugfs", 0, NULL);
    uint64_t targets[2] = {0};
    unsigned sockets = backend(targets);
    f = fopen(EINJ "available_error_type", "r");
    if (!f) { perror("EINJ available_error_type"); return 1; }
    char line[256];
    unsigned types = 0;
    while (fgets(line, sizeof(line), f)) {
        unsigned type;
        printf("EINJ available %s", line);
        if (sscanf(line, "%x", &type) == 1) types |= type;
    }
    fclose(f);
    if (types != 8 || !sockets) { puts("EINJ unsupported interface; refusing"); return 1; }
    for (unsigned s = 0; s < sockets; ++s) {
        if (!targets[s] || (targets[s] & 63)) { puts("EINJ socket target unavailable; refusing"); return 1; }
    }
    for (unsigned s = 0; s < sockets; ++s) {
        long before = ce_count();
        if (before < 0) { puts("EINJ no EDAC baseline; refusing"); return 1; }
        char target[32];
        snprintf(target, sizeof(target), "0x%" PRIx64, targets[s]);
        printf("EINJ baseline ce=%ld socket=%u target=%s\n", before, s, target);
        if (put("flags", "2") || put("param1", target) || put("param2", "0xfffffffffffff000") ||
            put("error_type", "8") || put("error_inject", "1")) {
            perror("EINJ injection"); backend(NULL); return 1;
        }
        puts("EINJ Linux execute/trigger/end returned success");
        backend(NULL);
        long after = before;
        for (int i = 0; i < 35 && after <= before; ++i) { sleep(1); after = ce_count(); }
        printf("EINJ result socket=%u ce_before=%ld ce_after=%ld\n", s, before, after);
        if (after <= before) { puts("EINJ_HWTEST FAILED: no hardware corrected-error evidence"); return 1; }
    }
    if (put("param1", "0x75bf0000")) { perror("EINJ invalid-target setup"); return 1; }
    if (!put("error_inject", "1") || errno != EINVAL) {
        puts("EINJ_HWTEST FAILED: non-scratch target not rejected with EINVAL"); backend(NULL); return 1;
    }
    puts("EINJ non-scratch target rejected with EINVAL");
    backend(NULL);
    puts("EINJ_HWTEST PASS: corrected-error reports on all socket targets; no #MC handoff claim");
    return 0;
}
