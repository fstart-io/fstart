/* SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * amd-ras: fstart's AMD RAS checks, for a disposable RAM-only Linux on a
 * test bench (tools/hwtest/linux-ras). One program for every AMD board:
 * SP3 (Rome, Milan), SP4r2 (Zeppelin) and the RS SoCs (Genoa, Siena).
 *
 *   amd-ras info                 the RAS tables, BERT's records, the SMM journal
 *   amd-ras smm                  every CPU: the SMI trigger, SMM's thresholds or PFEH
 *   amd-ras probe                one benign SEL event through SMM to the BMC
 *   amd-ras einj TYPE ADDR [N]   N memory injections of TYPE at ADDR, each consumed
 *   amd-ras einj-pcie TYPE SBDF  one PCIe injection at seg:bus:dev.fn
 *   amd-ras aer SBDF             a function's AER status registers
 *   amd-ras sp3-einj             SP3's correctable injection on its own targets
 *   amd-ras plant CPU BANK STATUS [ADDR]
 *                                write MCA_STATUS (HWCR.McStatusWrEn), which a
 *                                warm reset keeps for the next boot's BERT
 *   amd-ras plant-reset CPU BANK STATUS [ADDR]
 *                                the same, a mark in the RTC's CMOS of what was
 *                                planted, then at once a warm reset (CF9h)
 *   amd-ras plant-check          after a plant-reset: 0 if the BERT holds the
 *                                planted bank and status, 1 if not; 2 if there
 *                                was no plant (no mark). Clears the mark: one
 *                                plant per mark
 *   amd-ras mark-ue ADDR [UMC_ADDR] / ue-check
 *                                the same mark around an uncorrectable
 *                                injection at physical ADDR (UMC_ADDR: its
 *                                normalized address in its memory controller,
 *                                if known): ue-check 0 if the BERT has an
 *                                uncorrected or deferred record of it (its
 *                                address decoded, at least line-precise), 1
 *                                if not, 2 without a mark, 3 if only records
 *                                whose address cannot be compared (a memory
 *                                controller's without UMC_ADDR, a coarser
 *                                one): unverified
 *   amd-ras espi                 the FCH eSPI decodes and the KCS status ports
 *   amd-ras misc BANK            BANK's thresholding counter on every CPU
 *   amd-ras kcs PORT SPACING     one IPMI Get Device ID on a KCS channel
 *   amd-ras sel PORT SPACING     the SMM probe's SEL record, sent from the OS
 *
 * It never mounts or writes a disk. `einj` with an uncorrectable TYPE and
 * `plant` are meant to take the machine down; run them last.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <glob.h>
#include <inttypes.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/io.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <time.h>
#include <unistd.h>

#define EINJ "/sys/kernel/debug/apei/einj/"
#define DIAG_REQUEST 0x46535241530000d1ULL
#define HWCR 0xc0010015
#define MCSTATUS_WREN (1ULL << 18)

static unsigned failures;
#define FAIL(...) do { printf("RAS FAIL " __VA_ARGS__); putchar('\n'); ++failures; } while (0)

/* --- MSRs, CPUs and the SMM diagnostic ABI ------------------------------ */

static int msr_open(unsigned cpu, int flags)
{
    char path[64];
    snprintf(path, sizeof(path), "/dev/cpu/%u/msr", cpu);
    return open(path, flags);
}

static int rdmsr(int fd, uint32_t reg, uint64_t *value)
{
    return pread(fd, value, sizeof(*value), reg) == sizeof(*value) ? 0 : -1;
}

static int wrmsr(int fd, uint32_t reg, uint64_t value)
{
    return pwrite(fd, &value, sizeof(value), reg) == sizeof(value) ? 0 : -1;
}

static int pin(unsigned cpu)
{
    cpu_set_t cpus;
    CPU_ZERO(&cpus);
    CPU_SET(cpu, &cpus);
    return sched_setaffinity(0, sizeof(cpus), &cpus);
}

/* The online CPUs (/sys/devices/system/cpu/online, "0-3,8,10-11"): what
 * an every-CPU check must have observed. Returns their count, -1 if the
 * list cannot be read. */
#define MAX_CPUS 1024
static int online_cpus(unsigned char online[MAX_CPUS])
{
    memset(online, 0, MAX_CPUS);
    FILE *f = fopen("/sys/devices/system/cpu/online", "r");
    if (!f)
        return -1;
    char list[4096];
    int count = 0;
    if (!fgets(list, sizeof(list), f)) {
        fclose(f);
        return -1;
    }
    fclose(f);
    for (char *range = strtok(list, ",\n"); range; range = strtok(NULL, ",\n")) {
        unsigned first, last;
        int n = sscanf(range, "%u-%u", &first, &last);
        if (n == 1)
            last = first;
        else if (n != 2 || last < first)
            return -1;
        for (unsigned cpu = first; cpu <= last; ++cpu) {
            if (cpu >= MAX_CPUS)
                return -1;
            count += !online[cpu];
            online[cpu] = 1;
        }
    }
    return count;
}

struct diag { uint64_t a, c, d; };

/* The SMM handler's register-only diagnostic protocol (SW SMI D1,
 * fstart_platform_amd::smm::ras::DIAG_COMMAND): only the context carrying
 * the full request token is written. The SMI may arrive a few instructions
 * after the OUT, so the registers stay live in a spin after it; a request
 * that found no SMM handler is retried. */
static struct diag diag(uint64_t selector)
{
    struct diag r = {0};
    for (int attempt = 0; attempt < 20; ++attempt) {
        usleep(1000); /* let the FCH's previous SMI edge deassert */
        r = (struct diag){DIAG_REQUEST, selector, 0xb2};
        uint64_t spin = 100000;
        __asm__ volatile("outb %%al, %%dx\n"
                         "1: pause\n"
                         "dec %3\n"
                         "jnz 1b"
                         : "+a"(r.a), "+c"(r.c), "+d"(r.d), "+r"(spin)
                         :
                         : "memory");
        if (r.a != DIAG_REQUEST || r.d != 0xb2)
            break;
    }
    return r;
}

static int smi_port(void)
{
    if (ioperm(0xb2, 1, 1)) {
        perror("RAS ioperm 0xb2");
        return -1;
    }
    return 0;
}

/* --- EDAC and EINJ ------------------------------------------------------- */

static long edac(const char *what)
{
    char pattern[64];
    snprintf(pattern, sizeof(pattern), "/sys/devices/system/edac/mc/mc*/%s", what);
    glob_t paths = {0};
    if (glob(pattern, 0, NULL, &paths))
        return -1;
    long total = 0;
    for (size_t i = 0; i < paths.gl_pathc; ++i) {
        FILE *f = fopen(paths.gl_pathv[i], "r");
        long count;
        if (!f || fscanf(f, "%ld", &count) != 1) {
            if (f)
                fclose(f);
            globfree(&paths);
            return -1;
        }
        total += count;
        fclose(f);
    }
    globfree(&paths);
    return total;
}

static void debugfs(void)
{
    mount("debugfs", "/sys/kernel/debug", "debugfs", 0, NULL);
}

static int put(const char *name, const char *value)
{
    char path[128];
    snprintf(path, sizeof(path), EINJ "%s", name);
    int fd = open(path, O_WRONLY);
    if (fd < 0) {
        perror(path);
        return -1;
    }
    usleep(1000);
    /* busybox printf to debugfs fails silently; write(2) does not. */
    ssize_t n = write(fd, value, strlen(value));
    int error = errno;
    close(fd);
    if (n != (ssize_t)strlen(value)) {
        printf("RAS EINJ %s=%s: %s\n", name, value, strerror(error));
        errno = error;
        return -1;
    }
    return 0;
}

static unsigned available_types(void)
{
    FILE *f = fopen(EINJ "available_error_type", "r");
    if (!f) {
        perror("RAS EINJ available_error_type");
        return 0;
    }
    char line[256];
    unsigned types = 0;
    while (fgets(line, sizeof(line), f)) {
        unsigned type;
        if (sscanf(line, "%x", &type) == 1)
            types |= type;
    }
    fclose(f);
    printf("RAS EINJ available types %#x\n", types);
    return types;
}

/* One read of the cache line at `address` through /dev/mem: the access
 * that consumes an injected error. */
static void touch(uint64_t address)
{
    int fd = open("/dev/mem", O_RDONLY | O_SYNC);
    if (fd < 0) {
        perror("RAS /dev/mem");
        return;
    }
    uint64_t line[8];
    if (pread(fd, line, sizeof(line), address & ~63ULL) != sizeof(line))
        perror("RAS /dev/mem read");
    close(fd);
}

static int inject(unsigned type, const char *flags, const char *p1, const char *p2,
                  const char *p4)
{
    char t[16];
    snprintf(t, sizeof(t), "%#x", type);
    if (put("error_type", t) || put("flags", flags))
        return -1;
    if (p1 && put("param1", p1))
        return -1;
    if (p2 && put("param2", p2))
        return -1;
    if (p4 && put("param4", p4))
        return -1;
    return put("error_inject", "1");
}

/* --- ACPI tables and BERT ------------------------------------------------ */

static long table(const char *path, unsigned char *buffer, size_t size)
{
    FILE *f = fopen(path, "rb");
    if (!f)
        return -1;
    long n = (long)fread(buffer, 1, size, f);
    fclose(f);
    return n;
}

static uint32_t le32(const unsigned char *p) { return p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24; }
static uint64_t le64(const unsigned char *p) { return le32(p) | (uint64_t)le32(p + 4) << 32; }

static const unsigned char PROC_IA[16] = {0xb0, 0xa0, 0x3e, 0xdc, 0x44, 0xa1, 0x97, 0x47,
                                          0xb9, 0x5b, 0x53, 0xfa, 0x24, 0x2b, 0x6e, 0x1d};

/* The boot error region (ACPI 6.5 §18.3.1): generic error status blocks,
 * each of generic error data entries. Prints the MCA banks of the x86
 * processor sections. Returns the records found. Linux clears a block's
 * status once it printed it: after boot, its log is the evidence. */
/* An MCA record of the BERT a check is looking for. */
struct want {
    int (*match)(const struct want *, unsigned bank, uint64_t status, uint64_t addr,
                 uint64_t config, uint64_t ipid);
    unsigned bank;
    uint64_t value;
    /* A memory controller's (normalized) address, if `has_umc`. */
    uint64_t umc;
    int has_umc;
};

static unsigned bert_match(const struct want *want, unsigned *matches);

static unsigned bert(void)
{
    unsigned matches;
    return bert_match(NULL, &matches);
}

/* bert(), also counting the MCA records `want` matches. */
static unsigned bert_match(const struct want *want, unsigned *matches)
{
    *matches = 0;
    static unsigned char region[1 << 20];
    long length = table("/sys/firmware/acpi/tables/data/BERT", region, sizeof(region));
    if (length < 0) {
        puts("RAS BERT none");
        return 0;
    }
    unsigned records = 0;
    for (long block = 0; block + 20 <= length;) {
        const unsigned char *b = region + block;
        uint32_t status = le32(b), data = le32(b + 12), severity = le32(b + 16);
        if (!status && !data)
            break;
        printf("RAS BERT block @%ld status=%#x data=%u severity=%u\n", block, status, data, severity);
        for (uint32_t entry = 20; entry + 72 <= 20 + data && block + entry + 72 <= length;) {
            const unsigned char *e = b + entry;
            uint32_t sev = le32(e + 16), len = le32(e + 24);
            const unsigned char *section = e + 72;
            if (!memcmp(e, PROC_IA, 16) && block + entry + 72 + len <= length && len >= 64) {
                uint64_t valid = le64(section), lapic = le64(section + 8);
                unsigned errors = valid >> 2 & 0x3f, contexts = valid >> 8 & 0x3f;
                const unsigned char *ctx = section + 64 + 64 * errors;
                for (unsigned c = 0; c < contexts && ctx + 16 <= section + len; ++c) {
                    unsigned type = ctx[0] | ctx[1] << 8, size = ctx[2] | ctx[3] << 8;
                    uint32_t msr = le32(ctx + 4);
                    if (type == 1 && size >= 40) {
                        printf("RAS BERT mca severity=%u apic=%#" PRIx64 " bank=%u status=%016" PRIx64
                               " addr=%016" PRIx64 " ipid=%016" PRIx64 "\n",
                               sev, lapic, (msr >> 4) & 0xff, le64(ctx + 16), le64(ctx + 24), le64(ctx + 48));
                        *matches += want && want->match(want, (msr >> 4) & 0xff, le64(ctx + 16),
                                                        le64(ctx + 24), le64(ctx + 40),
                                                        le64(ctx + 48));
                    }
                    ctx += (16 + size + 15) & ~15u;
                }
                ++records;
            } else {
                printf("RAS BERT section severity=%u length=%u (not x86)\n", sev, len);
                ++records;
            }
            entry += 72 + len;
        }
        block += 20 + data;
    }
    printf("RAS BERT records=%u\n", records);
    return records;
}

static int info(void)
{
    static unsigned char buffer[1 << 20];
    const char *names[] = {"HEST", "BERT", "EINJ", "ERST", "SPMI"};
    for (unsigned i = 0; i < sizeof(names) / sizeof(*names); ++i) {
        char path[64];
        snprintf(path, sizeof(path), "/sys/firmware/acpi/tables/%s", names[i]);
        long n = table(path, buffer, sizeof(buffer));
        if (n < 0)
            printf("RAS table %s absent\n", names[i]);
        else
            printf("RAS table %s length=%ld oem=%.6s/%.8s\n", names[i], n, buffer + 10, buffer + 16);
    }
    long n = table("/sys/firmware/acpi/tables/HEST", buffer, sizeof(buffer));
    if (n >= 40) {
        uint32_t count = le32(buffer + 36);
        printf("RAS HEST sources=%u\n", count);
        for (long off = 40; off + 4 <= n && count--;) {
            unsigned type = buffer[off] | buffer[off + 1] << 8, id = buffer[off + 2] | buffer[off + 3] << 8;
            long size;
            switch (type) {
            case 0: size = 40 + 28 * buffer[off + 32]; printf("RAS HEST %u: MCE flags=%#x banks=%u\n", id, buffer[off + 6], buffer[off + 32]); break;
            case 1: size = 48 + 28 * buffer[off + 44]; printf("RAS HEST %u: CMC flags=%#x threshold=%u banks=%u\n", id, buffer[off + 6], le32(buffer + off + 36), buffer[off + 44]); break;
            case 9: size = 64; printf("RAS HEST %u: GHES related=%#x notify=%u block=%#" PRIx64 "\n", id, buffer[off + 4] | buffer[off + 5] << 8, buffer[off + 32], le64(buffer + off + 24)); break;
            case 10: size = 92; printf("RAS HEST %u: GHESv2 notify=%u\n", id, buffer[off + 32]); break;
            default: printf("RAS HEST %u: type %u\n", id, type); size = n; break;
            }
            off += size;
        }
    }
    bert();
    return 0;
}

/* --- the subcommands ---------------------------------------------------- */

/* On every CPU: the trigger I/O cycle B2/80h armed, the cloak and deferred
 * masks clear (the OS keeps every bank), and either PFEH on in SMM with
 * #MC redirected, or SMM's thresholding counters locked in the banks'
 * MCA_MISC0 (IntType SMI). */
static int smm_check(void)
{
    if (smi_port())
        return 1;
    static unsigned char online[MAX_CPUS];
    int expected = online_cpus(online);
    if (expected <= 0) {
        FAIL("the online CPUs");
        return 1;
    }
    unsigned present = 0, armed_cpus = 0, pfeh_cpus = 0, counters = 0, unscoped = 0;
    uint64_t blocks = 0, unscoped_cpu0 = 0;
    for (unsigned cpu = 0; cpu < MAX_CPUS; ++cpu) {
        if (!online[cpu])
            continue;
        int fd = msr_open(cpu, O_RDONLY);
        if (fd < 0) {
            FAIL("cpu %u online, its MSRs unreadable", cpu);
            continue;
        }
        if (pin(cpu)) {
            FAIL("cpu %u online, not schedulable", cpu);
            close(fd);
            continue;
        }
        ++present;
        uint64_t redir = 0, trigger = 0, cap = 0;
        rdmsr(fd, 0xc0010022, &redir);
        rdmsr(fd, 0xc0010056, &trigger);
        rdmsr(fd, 0x179, &cap);
        unsigned locked = 0, banks = cap & 0xff;
        for (unsigned bank = 0; bank < banks && bank < 64; ++bank) {
            uint64_t misc = 0;
            rdmsr(fd, 0xc0002003 + 16 * bank, &misc);
            locked += (misc >> 61 & 1) && (misc >> 49 & 3) == 2;
        }
        struct diag smm = diag(0);
        int pfeh = (smm.a & 1) && (redir & 0x300) == 0x200;
        int armed = (trigger & 0x06ffffff) == 0x028000b2 && smm.c == 0 && smm.d == 0 &&
                    (pfeh || locked);
        if (cpu < 4 || !armed)
            printf("RAS cpu=%u redir=%016" PRIx64 " trigger=%016" PRIx64 " locked_thresholds=%u/%u"
                   " pfeh_smm=%016" PRIx64 " cloak=%016" PRIx64 " mask=%016" PRIx64 " %s\n",
                   cpu, redir, trigger, locked, banks, smm.a, smm.c, smm.d, armed ? "ARMED" : "NOT_ARMED");
        armed_cpus += armed;
        pfeh_cpus += pfeh;
        /* The counters SMM armed on this CPU, the blocks on every CPU,
         * the banks it left alone (no known scope). */
        struct diag own = diag(7);
        counters += __builtin_popcountll(own.a);
        unscoped += __builtin_popcountll(own.d);
        blocks = own.c;
        if (cpu == 0)
            unscoped_cpu0 = own.d;
        close(fd);
    }
    pin(0);
    struct diag journal = diag(4), layout = diag(3);
    printf("RAS SMM online=%d observed=%u armed=%u pfeh=%u journal=%" PRIu64 " dropped=%" PRIu64
           " refused=%" PRIu64 " save_state=%" PRIu64 "/%" PRIu64 "/%#" PRIx64 "\n", expected, present,
           armed_cpus, pfeh_cpus, journal.a, journal.c, journal.d, layout.a, layout.c, layout.d);
    printf("RAS SMM counters=%u blocks=%" PRIu64 " unscoped=%u cpu0_unscoped_banks=%#" PRIx64 "\n",
           counters, blocks, unscoped, unscoped_cpu0);
    if (present != (unsigned)expected || armed_cpus != (unsigned)expected)
        FAIL("not every online CPU is observed and armed");
    return failures ? 1 : 0;
}

static int probe(void)
{
    if (smi_port() || pin(0))
        return 1;
    struct timespec start, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    struct diag result = diag(1);
    for (unsigned i = 0; i < 8 && (result.a == 1 || result.a == 4); ++i)
        result = diag(2);
    clock_gettime(CLOCK_MONOTONIC, &end);
    uint64_t us = (end.tv_sec - start.tv_sec) * 1000000ULL + (end.tv_nsec - start.tv_nsec) / 1000;
    printf("RAS SEL_PROBE result=%" PRIu64 " elapsed_us=%" PRIu64 " (2=success,3=failure)\n", result.a, us);
    return result.a == 2 ? 0 : 1;
}

static uint64_t journal(void)
{
    if (smi_port() || pin(0))
        return UINT64_MAX;
    return diag(4).a;
}

/* N memory injections at ADDR, each consumed by a read. A correctable
 * TYPE must raise EDAC's CE count each time (Linux polls the banks: set
 * check_interval to 1); with N at least the board's threshold, the SMM
 * journal must grow by the thresholding events. An uncorrectable TYPE
 * takes the machine down at the read. */
static int einj(unsigned type, uint64_t address, unsigned count)
{
    debugfs();
    if (!(available_types() & type)) {
        FAIL("EINJ type %#x not available", type);
        return 1;
    }
    char p1[32];
    snprintf(p1, sizeof(p1), "%#" PRIx64, address);
    uint64_t before_journal = journal();
    long total = edac("ce_count");
    if ((type & 0x8) && total < 0) {
        FAIL("no EDAC CE counters to check the injections with");
        return 1;
    }
    unsigned seen = 0;
    for (unsigned i = 0; i < count; ++i) {
        long before = edac("ce_count");
        if (inject(type, "2", p1, "0xfffffffffffff000", NULL)) {
            FAIL("EINJ inject %u", i);
            return 1;
        }
        printf("RAS EINJ %u type=%#x address=%s injected, consuming\n", i, type, p1);
        fflush(stdout);
        touch(address);
        if (type & 0x8) {
            long after = before;
            for (int t = 0; t < 40 && before >= 0 && after >= 0 && after <= before; ++t) {
                usleep(100000);
                after = edac("ce_count");
            }
            /* Both counters read: a failed read is no observation. */
            if (before < 0 || after < 0)
                FAIL("EINJ %u: EDAC CE count unreadable (%ld -> %ld)", i, before, after);
            else
                seen += after > before;
            printf("RAS EINJ %u ce %ld -> %ld\n", i, before, after);
        }
    }
    sleep(1);
    uint64_t after_journal = journal();
    printf("RAS EINJ done type=%#x count=%u ce_total %ld -> %ld seen=%u journal %" PRIu64 " -> %" PRIu64 "\n",
           type, count, total, edac("ce_count"), seen, before_journal, after_journal);
    if ((type & 0x8) && seen != count)
        FAIL("EDAC missed %u of %u corrected errors", count - seen, count);
    return failures ? 1 : 0;
}

/* The AER status of a PCI function (its extended capability 0001h):
 * uncorrectable and correctable status, root error status (ports). */
static void aer_status(unsigned seg, unsigned bus, unsigned dev, unsigned fn, const char *when)
{
    char path[96];
    snprintf(path, sizeof(path), "/sys/bus/pci/devices/%04x:%02x:%02x.%x/config", seg, bus, dev, fn);
    static unsigned char config[4096];
    if (table(path, config, sizeof(config)) < 4096) {
        printf("RAS AER %s: no extended config space\n", path);
        return;
    }
    for (unsigned off = 0x100, n = 0; off && off < 4096 && n < 64; ++n) {
        uint32_t header = le32(config + off);
        if ((header & 0xffff) == 1) {
            printf("RAS AER %s %04x:%02x:%02x.%x @%#x uncorrectable=%08x mask=%08x severity=%08x"
                   " correctable=%08x mask=%08x root_command=%08x root_status=%08x source=%08x\n",
                   when, seg, bus, dev, fn, off, le32(config + off + 4), le32(config + off + 8),
                   le32(config + off + 12), le32(config + off + 16), le32(config + off + 20),
                   le32(config + off + 0x2c), le32(config + off + 0x30), le32(config + off + 0x34));
            return;
        }
        off = header >> 20;
    }
    printf("RAS AER %04x:%02x:%02x.%x: no AER capability\n", seg, bus, dev, fn);
}

/* The kernel log from now on (/dev/kmsg, at its end). */
static int kmsg_from_now(void)
{
    int fd = open("/dev/kmsg", O_RDONLY | O_NONBLOCK);
    if (fd >= 0)
        lseek(fd, 0, SEEK_END);
    return fd;
}

/* Milliseconds on the monotonic clock. */
static long long monotonic_ms(void)
{
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000LL + now.tv_nsec / 1000000;
}

/* Whether a record after `fd`'s start holds `needle` (and `also`, if
 * given) within `seconds`, by the monotonic clock: a log that never runs
 * dry does not hold it up. */
static int kmsg_wait(int fd, const char *needle, const char *also, int seconds)
{
    char record[2048];
    for (long long deadline = monotonic_ms() + seconds * 1000LL; monotonic_ms() < deadline;) {
        ssize_t n = read(fd, record, sizeof(record) - 1);
        if (n < 0 && errno == EAGAIN) {
            usleep(100000);
            continue;
        }
        if (n <= 0)
            continue;
        record[n] = 0;
        if (strstr(record, needle) && (!also || strstr(record, also))) {
            printf("RAS kmsg: %s", strchr(record, ';') ? strchr(record, ';') + 1 : record);
            return 1;
        }
    }
    return 0;
}

/* One PCIe injection at SBDF: the EINJ write must succeed, and the AER
 * driver must report an error message of the injected severity from that
 * function after it (ACPI 6.5 table 18.30: 40h correctable, 80h
 * uncorrectable non-fatal, 100h uncorrectable fatal; Linux's AER prints
 * "<severity> error message received from <SBDF>", "Error" from Linux 7
 * on), not one an earlier test left in the log. */
static int einj_pcie(unsigned type, const char *where)
{
    unsigned seg, bus, dev, fn;
    if (sscanf(where, "%x:%x:%x.%x", &seg, &bus, &dev, &fn) != 4) {
        FAIL("SBDF %s", where);
        return 1;
    }
    const char *severity = type == 0x40    ? "Correctable"
                           : type == 0x80  ? "Uncorrectable (Non-Fatal)"
                           : type == 0x100 ? "Uncorrectable (Fatal)"
                                           : NULL;
    if (!severity) {
        FAIL("EINJ type %#x is not a PCIe one", type);
        return 1;
    }
    debugfs();
    if (!(available_types() & type)) {
        FAIL("EINJ type %#x not available", type);
        return 1;
    }
    /* ACPI 6.5 table 18.30: segment, bus, device/function, reserved. */
    char p4[32];
    snprintf(p4, sizeof(p4), "%#x", seg << 24 | bus << 16 | (dev << 3 | fn) << 8);
    aer_status(seg, bus, dev, fn, "before");
    int log = kmsg_from_now();
    if (log < 0) {
        FAIL("/dev/kmsg");
        return 1;
    }
    if (inject(type, "4", NULL, NULL, p4)) {
        FAIL("EINJ PCIe inject");
        close(log);
        return 1;
    }
    printf("RAS EINJ PCIe type=%#x sbdf=%s param4=%s injected\n", type, where, p4);
    /* The injected error hits a later transaction of the function, and its
     * report can take seconds (the Onyx: up to about ten): read its config
     * space once a second, for traffic, while waiting for the report. */
    char needle[128], path[96];
    snprintf(needle, sizeof(needle), "rror message received from %04x:%02x:%02x.%x", seg, bus, dev,
             fn);
    /* The severity, then "error" (Linux 6) or "Error" (Linux 7): with the
     * space after it, "Correctable " is not in "Uncorrectable (...) ". */
    char severe[48];
    snprintf(severe, sizeof(severe), "%s ", severity);
    snprintf(path, sizeof(path), "/sys/bus/pci/devices/%04x:%02x:%02x.%x/config", seg, bus, dev, fn);
    int reported = 0;
    for (int second = 0; second < 30 && !reported; ++second) {
        unsigned char header[64];
        table(path, header, sizeof(header));
        reported = kmsg_wait(log, needle, severe, 1);
        if (reported)
            printf("RAS EINJ PCIe reported after about %d s\n", second + 1);
    }
    if (!reported)
        FAIL("no %s AER report from %04x:%02x:%02x.%x in 30 s after the injection", severity, seg,
             bus, dev, fn);
    close(log);
    aer_status(seg, bus, dev, fn, "after");
    return failures ? 1 : 0;
}

/* The fixed SP3 EINJ page of fstart's Rome firmware: one firmware-owned
 * correctable target per socket (crates/platform-amd/src/sp3/einj.rs). */
#define SP3_BASE 0x75bf0000ULL
static int sp3_einj(void)
{
    unsigned char header[16];
    if (table("/sys/firmware/acpi/tables/FACP", header, sizeof(header)) < 16 || memcmp(header + 10, "FSTART", 6)) {
        puts("RAS SP3 EINJ: not fstart firmware; refusing");
        return 1;
    }
    int fd = open("/dev/mem", O_RDONLY | O_SYNC);
    if (fd < 0)
        return 1;
    volatile uint64_t *p = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, SP3_BASE);
    if (p == MAP_FAILED) {
        close(fd);
        return 1;
    }
    uint64_t sockets = p[66] <= 2 ? p[66] : 0, targets[2] = {p[64], p[65]};
    printf("RAS SP3 EINJ supported=%" PRIx64 " sockets=%" PRIu64 "\n", p[0], sockets);
    munmap((void *)p, 4096);
    close(fd);
    for (unsigned s = 0; s < sockets; ++s) {
        if (!targets[s] || targets[s] & 63) {
            FAIL("socket %u target", s);
            return 1;
        }
        if (einj(8, targets[s], 1))
            return 1;
    }
    if (!sockets)
        FAIL("no SP3 EINJ targets");
    return failures ? 1 : 0;
}

/* A mark across the warm reset, in the RTC's upper CMOS bank (ports
 * 72h/73h; nothing of fstart's uses it): a magic byte, what was done, the
 * bank and the status as the bank read it back. Battery backed: a run
 * that never came back leaves it, and the next check reports it. */
#define MARK 0xe0
#define MARK_MAGIC 0xf5
enum { MARK_PLANT = 1, MARK_UE = 2 };

static int cmos(void)
{
    if (ioperm(0x72, 2, 1)) {
        perror("RAS ioperm 0x72");
        return -1;
    }
    return 0;
}

static unsigned char cmos_get(unsigned index) { outb(index, 0x72); return inb(0x73); }
static void cmos_set(unsigned index, unsigned char value) { outb(index, 0x72); outb(value, 0x73); }

static int mark(unsigned what, unsigned bank, uint64_t status)
{
    if (cmos())
        return -1;
    cmos_set(MARK + 1, what);
    cmos_set(MARK + 2, bank);
    for (unsigned i = 0; i < 8; ++i)
        cmos_set(MARK + 3 + i, status >> (8 * i));
    cmos_set(MARK, MARK_MAGIC);
    return cmos_get(MARK) == MARK_MAGIC ? 0 : -1;
}

/* The mark, cleared: 0 none, else what was done, with its bank and status. */
static unsigned take_mark(unsigned *bank, uint64_t *status)
{
    if (cmos() || cmos_get(MARK) != MARK_MAGIC)
        return 0;
    unsigned what = cmos_get(MARK + 1);
    *bank = cmos_get(MARK + 2);
    *status = 0;
    for (unsigned i = 0; i < 8; ++i)
        *status |= (uint64_t)cmos_get(MARK + 3 + i) << (8 * i);
    cmos_set(MARK, 0);
    return what;
}

/* A synthetic MCA_STATUS (and ADDR) in BANK of CPU, written with
 * HWCR.McStatusWrEn; with PFEH the write traps to SMM first, which hands
 * it back (and stops observing that CPU). The bank keeps it across a warm
 * reset, for the next boot's BERT. */
static int plant(unsigned cpu, unsigned bank, uint64_t status, uint64_t address, int reset)
{
    int fd = msr_open(cpu, O_RDWR);
    if (fd < 0 || pin(cpu)) {
        FAIL("cpu %u msr", cpu);
        return 1;
    }
    uint64_t hwcr, readback = 0;
    if (rdmsr(fd, HWCR, &hwcr) || wrmsr(fd, HWCR, hwcr | MCSTATUS_WREN)) {
        FAIL("HWCR");
        return 1;
    }
    uint32_t base = 0xc0002000 + bank * 16;
    int ok = !wrmsr(fd, base + 2, address) && !wrmsr(fd, base + 1, status);
    wrmsr(fd, HWCR, hwcr);
    rdmsr(fd, base + 1, &readback);
    close(fd);
    /* Some status bits are read-only (Zeppelin's UMC keeps bit 24 clear):
     * what counts is a valid, uncorrected entry. */
    ok = ok && (readback >> 63 & 1) && (readback >> 61 & 1);
    printf("RAS PLANT cpu=%u bank=%u status=%016" PRIx64 " addr=%016" PRIx64 " readback=%016" PRIx64 " %s\n",
           cpu, bank, status, address, readback, ok ? "OK" : "FAILED");
    if (reset && ok && mark(MARK_PLANT, bank, readback)) {
        FAIL("the CMOS mark");
        return 1;
    }
    if (reset && ok && !ioperm(0xcf9, 1, 1)) {
        /* At once: Linux's poller would log and clear the bank. CF9h
         * 06h, the FADT's reset: a warm reset, which keeps MCA_STATUS. */
        puts("RAS PLANT warm reset");
        outb(0x06, 0xcf9);
        sleep(5);
        puts("RAS PLANT reset did not happen");
        return 1;
    }
    return ok ? 0 : 1;
}

/* The FCH eSPI controller's I/O decodes (FEC20000h: 40h enables, 44h/48h
 * the four I/O bases, 4Ch their sizes) and what the KCS status ports read. */
static int espi(void)
{
    int fd = open("/dev/mem", O_RDONLY | O_SYNC);
    if (fd >= 0) {
        volatile uint32_t *r = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0xfec20000);
        if (r != MAP_FAILED) {
            printf("RAS ESPI 30=%08x 40=%08x 44=%08x 48=%08x 4c=%08x 50=%08x 54=%08x 68=%08x\n",
                   r[0x30 / 4], r[0x40 / 4], r[0x44 / 4], r[0x48 / 4], r[0x4c / 4], r[0x50 / 4],
                   r[0x54 / 4], r[0x68 / 4]);
            printf("RAS ESPI 80=%08x 84=%08x 88=%08x 8c=%08x 90=%08x 94=%08x 98=%08x 9c=%08x a0=%08x\n",
                   r[0x80 / 4], r[0x84 / 4], r[0x88 / 4], r[0x8c / 4], r[0x90 / 4], r[0x94 / 4],
                   r[0x98 / 4], r[0x9c / 4], r[0xa0 / 4]);
            munmap((void *)r, 4096);
        }
        close(fd);
    }
    if (ioperm(0xca0, 16, 1)) {
        perror("RAS ioperm 0xca0");
        return 1;
    }
    printf("RAS KCS ca2/ca3=%02x/%02x ca8/cac=%02x/%02x ca4/ca5=%02x/%02x\n", inb(0xca2), inb(0xca3),
           inb(0xca8), inb(0xcac), inb(0xca4), inb(0xca5));
    return 0;
}

/* One IPMI Get Device ID over the KCS interface at PORT (status/command
 * SPACING ports above), from the OS: does the channel answer, and how fast.
 * Only on a channel no OS driver uses (the test kernel has none). */
static int kcs_wait(unsigned port, int mask, int value, struct timespec *start, uint64_t *polls)
{
    for (;;) {
        int status = inb(port);
        ++*polls;
        if ((status & mask) == value)
            return status;
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        if ((now.tv_sec - start->tv_sec) * 1000000000LL + (now.tv_nsec - start->tv_nsec) > 2000000000LL)
            return -1;
        usleep(10);
    }
}

/* One request over the KCS interface (IPMI 2.0 §9.15); prints the response
 * and returns its completion code (-1: transport failure). */
static int kcs_request(unsigned data, unsigned spacing, const unsigned char *request, int length)
{
    unsigned cmd = data + spacing;
    if (ioperm(data < cmd ? data : cmd, spacing + 1, 1)) {
        perror("RAS ioperm kcs");
        return -1;
    }
    struct timespec start, end;
    uint64_t polls = 0;
    clock_gettime(CLOCK_MONOTONIC, &start);
    printf("RAS KCS %#x/%#x status=%02x\n", data, cmd, inb(cmd));
    unsigned char response[32];
    int n = 0, status;
    if (kcs_wait(cmd, 2, 0, &start, &polls) < 0) goto timeout;
    outb(0x61, cmd);                                   /* WRITE_START */
    for (int i = 0; i < length; ++i) {
        if (kcs_wait(cmd, 2, 0, &start, &polls) < 0) goto timeout;
        if (inb(cmd) & 1) inb(data);
        if (i == length - 1) {
            outb(0x62, cmd);                           /* WRITE_END */
            if (kcs_wait(cmd, 2, 0, &start, &polls) < 0) goto timeout;
            if (inb(cmd) & 1) inb(data);
        }
        outb(request[i], data);
    }
    for (;;) {
        if ((status = kcs_wait(cmd, 2, 0, &start, &polls)) < 0) goto timeout;
        if ((status >> 6) == 0) break;                 /* IDLE */
        if ((status >> 6) != 1) { printf("RAS KCS state %02x\n", status); return -1; }
        if (kcs_wait(cmd, 1, 1, &start, &polls) < 0) goto timeout;
        int byte = inb(data);
        if (n < (int)sizeof(response)) response[n++] = byte;
        outb(0x68, data);                              /* READ */
    }
    if (inb(cmd) & 1) inb(data);
    clock_gettime(CLOCK_MONOTONIC, &end);
    printf("RAS KCS response %d bytes in %lld us, %" PRIu64 " polls:", n,
           ((end.tv_sec - start.tv_sec) * 1000000000LL + (end.tv_nsec - start.tv_nsec)) / 1000, polls);
    for (int i = 0; i < n; ++i) printf(" %02x", response[i]);
    putchar('\n');
    return n >= 3 ? response[2] : -1;
timeout:
    printf("RAS KCS timeout, status=%02x after %" PRIu64 " polls\n", inb(cmd), polls);
    return -1;
}

/* Get Device ID: does the channel answer, and how fast. Only on a channel
 * no OS driver uses (the test kernel has none). */
static int kcs(unsigned data, unsigned spacing)
{
    const unsigned char request[] = {0x06 << 2, 0x01};
    return kcs_request(data, spacing, request, sizeof(request)) == 0 ? 0 : 1;
}

/* Platform Event Message (IPMI 2.0 §29.3) of the SMM probe's event, from
 * the OS: the generator, then the event message. */
static int event(unsigned data, unsigned spacing)
{
    const unsigned char request[] = {0x04 << 2, 0x02, 0x21, 4, 0x12, 0xff, 0x6f, 0xa3, 'F', 'S'};
    return kcs_request(data, spacing, request, sizeof(request)) == 0 ? 0 : 1;
}

/* Get SEL Info (IPMI 2.0 §31.2), from the OS. */
static int sel_info(unsigned data, unsigned spacing)
{
    const unsigned char request[] = {0x0a << 2, 0x40};
    return kcs_request(data, spacing, request, sizeof(request)) == 0 ? 0 : 1;
}

/* Add SEL Entry of the record the SMM handler's probe sends
 * (fstart_platform_amd::smm::ras::sel), from the OS. */
static int sel(unsigned data, unsigned spacing)
{
    const unsigned char request[] = {0x0a << 2, 0x44, 0, 0, 2, 0, 0, 0, 0, 0x21, 0, 4, 0x12, 0xff,
                                     0x6f, 0xa3, 'F', 'S'};
    return kcs_request(data, spacing, request, sizeof(request)) == 0 ? 0 : 1;
}

static int same_bank_status(const struct want *want, unsigned bank, uint64_t status, uint64_t addr,
                            uint64_t config, uint64_t ipid)
{
    (void)addr, (void)config, (void)ipid;
    return bank == want->bank && status == want->value;
}

/* MCA_STATUS: valid, uncorrected or deferred, with a valid address. */
static int uncorrected_with_address(uint64_t status)
{
    return status >> 63 && (status >> 61 & 1 || status >> 44 & 1) && status >> 58 & 1;
}

/* An MCA record's error address and its least significant valid bit, as
 * Linux decodes it (smca_extract_err_addr): MCA_ADDR[55:LSB], LSB in
 * MCA_ADDR[61:56]; where MCA_CONFIG[8] (McaLsbInStatusSupported) is set,
 * MCA_ADDR[56:LSB] with LSB in MCA_STATUS[29:24]. MCA_ADDR's metadata
 * bits are never compared. */
static uint64_t error_address(uint64_t status, uint64_t addr, uint64_t config, unsigned *lsb)
{
    unsigned top = 55;
    *lsb = addr >> 56 & 0x3f;
    if (config >> 8 & 1) {
        *lsb = status >> 24 & 0x3f;
        top = 56;
    }
    if (*lsb > top)
        return 0;
    return addr & (~0ULL >> (63 - top)) & ~((1ULL << *lsb) - 1);
}

/* Whether a record's address is precise enough to tell the injected 64-byte
 * line from its neighbours. */
#define LINE_BITS 6

enum ue_kind { UE_OTHER, UE_LOAD_STORE, UE_UMC };

/* A load-store bank (MCA_IPID HardwareID B0h, McaType 0, 10h on Genoa),
 * whose MCA_ADDR is the physical address of the access, or a memory
 * controller's (HardwareID 96h), whose MCA_ADDR is the normalized DRAM
 * address. */
static enum ue_kind ue_kind(uint64_t ipid)
{
    unsigned hwid = ipid >> 32 & 0xfff, type = ipid >> 48 & 0xffff;
    if (hwid == 0xb0 && (type == 0 || type == 0x10))
        return UE_LOAD_STORE;
    return hwid == 0x96 ? UE_UMC : UE_OTHER;
}

/* The address `want` gives for a record of `kind`, if it has one. */
static int wanted_address(const struct want *want, enum ue_kind kind, uint64_t *address)
{
    if (kind == UE_LOAD_STORE) {
        *address = want->value;
        return 1;
    }
    *address = want->umc;
    return kind == UE_UMC && want->has_umc;
}

/* Whether the record's address is the system physical address: where the
 * bank reports one (MCA_CONFIG[11], PADDRV), only with MCA_STATUS[54]. */
static int physical(uint64_t status, uint64_t config)
{
    return !(config >> 11 & 1) || status >> 54 & 1;
}

/* The injected error, its address compared: valid, uncorrected or
 * deferred, with an address of at least a cache line's precision (a
 * physical one for a load-store bank) in the line of the one wanted. */
static int uncorrected_at(const struct want *want, unsigned bank, uint64_t status, uint64_t addr,
                          uint64_t config, uint64_t ipid)
{
    (void)bank;
    enum ue_kind kind = ue_kind(ipid);
    uint64_t wanted, line = ~((1ULL << LINE_BITS) - 1);
    unsigned lsb;
    if (!uncorrected_with_address(status) || !wanted_address(want, kind, &wanted))
        return 0;
    if (kind == UE_LOAD_STORE && !physical(status, config))
        return 0;
    uint64_t found = error_address(status, addr, config, &lsb);
    return lsb <= LINE_BITS && (found & line) == (wanted & line);
}

/* A record of the injection's kind whose address cannot be compared: a
 * memory controller's with no normalized address given, or one too coarse
 * (its LSB above the cache line) or, for a load-store bank, not a physical
 * address. It cannot prove the injection: unverified. */
static int uncorrected_unverifiable(const struct want *want, unsigned bank, uint64_t status,
                                    uint64_t addr, uint64_t config, uint64_t ipid)
{
    (void)bank;
    enum ue_kind kind = ue_kind(ipid);
    uint64_t wanted;
    unsigned lsb;
    if (!uncorrected_with_address(status) || kind == UE_OTHER)
        return 0;
    if (!wanted_address(want, kind, &wanted))
        return 1;
    error_address(status, addr, config, &lsb);
    return lsb > LINE_BITS || (kind == UE_LOAD_STORE && !physical(status, config));
}

/* After plant-reset's warm reset: the BERT must hold the planted bank with
 * the status it read back. 2: no mark, nothing was planted. One plant per
 * mark: the mark is gone afterwards whatever the result. */
static int plant_check(void)
{
    unsigned bank, matches;
    uint64_t status;
    unsigned what = take_mark(&bank, &status);
    if (what != MARK_PLANT) {
        printf("RAS PLANT no mark%s\n", what ? " of a plant" : "");
        return what ? 1 : 2;
    }
    printf("RAS PLANT marked bank=%u status=%016" PRIx64 "\n", bank, status);
    const struct want planted = {same_bank_status, bank, status, 0, 0};
    bert_match(&planted, &matches);
    if (!matches)
        FAIL("the BERT has no record of bank %u with status %016" PRIx64, bank, status);
    return failures ? 1 : 0;
}

/* The mark of an uncorrectable injection at `address`, kept in the mark's
 * status bytes, and, if `has_umc`, its normalized address in its memory
 * controller in the eight bytes after them; the mark's bank byte says
 * whether there is one (an address 0 is an address). */
static int mark_ue(uint64_t address, int has_umc, uint64_t umc)
{
    if (mark(MARK_UE, has_umc ? 1 : 0, address))
        return 1;
    for (unsigned i = 0; i < 8; ++i)
        cmos_set(MARK + 11 + i, has_umc ? umc >> (8 * i) : 0);
    return 0;
}

/* After an uncorrectable injection took the machine down: the BERT must
 * hold an uncorrected or deferred record of the injected address, not
 * just any record (uncorrected_at). 2: no mark. 3: only records whose
 * address cannot be compared (uncorrected_unverifiable): unverified, not
 * passed. */
static int ue_check(void)
{
    unsigned has_umc, matches, unverifiable;
    uint64_t address, umc = 0;
    unsigned what = take_mark(&has_umc, &address);
    if (what != MARK_UE)
        return what ? 1 : 2;
    for (unsigned i = 0; i < 8; ++i)
        umc |= (uint64_t)cmos_get(MARK + 11 + i) << (8 * i);
    if (has_umc)
        printf("RAS UE marked address=%#" PRIx64 " umc=%#" PRIx64 "\n", address, umc);
    else
        printf("RAS UE marked address=%#" PRIx64 " umc=none\n", address);
    const struct want injected = {uncorrected_at, 0, address, umc, has_umc != 0};
    const struct want unproven = {uncorrected_unverifiable, 0, address, umc, has_umc != 0};
    if (!bert_match(&injected, &matches)) {
        FAIL("the BERT is empty after the uncorrectable injection");
    } else if (!matches) {
        bert_match(&unproven, &unverifiable);
        if (unverifiable) {
            printf("RAS UE UNVERIFIED: %u record(s) of the injection's kind whose address cannot "
                   "be compared (no UMC address given, coarser than a line, or not physical)\n",
                   unverifiable);
            return 3;
        }
        FAIL("the BERT has no uncorrected or deferred record of %#" PRIx64, address);
    }
    return failures ? 1 : 0;
}

/* The checks' decisions on synthetic records, no hardware: build.sh runs
 * it. 0 if every case holds. */
static int selftest(void)
{
    const uint64_t ue = 1ULL << 63 | 1ULL << 61 | 1ULL << 58;
    const uint64_t deferred = 1ULL << 63 | 1ULL << 44 | 1ULL << 58;
    const uint64_t ls = 0x10ULL << 48 | 0xb0ULL << 32, ls_zen3 = 0xb0ULL << 32, umc = 0x96ULL << 32;
    const uint64_t at = 0x140000000ULL;
    const struct want no_umc = {uncorrected_at, 0, at, 0, 0};
    const struct want umc_0 = {uncorrected_at, 0, at, 0, 1};
    const struct want umc_at = {uncorrected_at, 0, at, 0x2000040, 1};
    struct {
        const char *what;
        const struct want *want;
        uint64_t status, addr, config, ipid;
        int match, unverifiable;
    } cases[] = {
        {"LS in the line", &no_umc, ue, at + 0x3f, 0, ls, 1, 0},
        {"LS (Zen 3) in the line, deferred", &no_umc, deferred, at + 8, 0, ls_zen3, 1, 0},
        {"LS metadata: LSB 6 in MCA_ADDR[61:56]", &no_umc, ue, 6ULL << 56 | at, 0, ls, 1, 0},
        {"LS in the next line", &no_umc, ue, at + 0x40, 0, ls, 0, 0},
        {"LS a page coarse", &no_umc, ue, 12ULL << 56 | at, 0, ls, 0, 1},
        {"LS without AddrV", &no_umc, ue & ~(1ULL << 58), at, 0, ls, 0, 0},
        {"LS corrected", &no_umc, 1ULL << 63 | 1ULL << 58, at, 0, ls, 0, 0},
        {"LS PADDRV supported, not set", &no_umc, ue, at, 1ULL << 11, ls, 0, 1},
        {"LS PADDRV set", &no_umc, ue | 1ULL << 54, at, 1ULL << 11, ls, 1, 0},
        {"LS LSB in MCA_STATUS[29:24]", &no_umc, ue | 6ULL << 24, at | 0x1fULL << 57, 1ULL << 8, ls, 1, 0},
        {"LS LSB in MCA_STATUS, bit 56 an address bit", &no_umc, ue | 6ULL << 24, at | 1ULL << 56, 1ULL << 8, ls, 0, 0},
        {"LS LSB in MCA_STATUS, coarse", &no_umc, ue | 12ULL << 24, at, 1ULL << 8, ls, 0, 1},
        {"UMC, no address given", &no_umc, ue, 0x2000040, 0, umc, 0, 1},
        {"UMC at address 0, given", &umc_0, ue, 0, 0, umc, 1, 0},
        {"UMC elsewhere, 0 given", &umc_0, ue, 0x40, 0, umc, 0, 0},
        {"UMC at the given address", &umc_at, ue, 0x2000040, 0, umc, 1, 0},
        {"another bank", &umc_at, ue, at, 0, 0x2eULL << 32, 0, 0},
    };
    for (unsigned i = 0; i < sizeof(cases) / sizeof(*cases); ++i) {
        const struct want unproven = {uncorrected_unverifiable, 0, cases[i].want->value,
                                      cases[i].want->umc, cases[i].want->has_umc};
        int match = cases[i].want->match(cases[i].want, 0, cases[i].status, cases[i].addr,
                                         cases[i].config, cases[i].ipid);
        int unverifiable = unproven.match(&unproven, 0, cases[i].status, cases[i].addr,
                                          cases[i].config, cases[i].ipid);
        if (match != cases[i].match || unverifiable != cases[i].unverifiable)
            FAIL("selftest %s: match %d unverifiable %d", cases[i].what, match, unverifiable);
    }
    printf("RAS SELFTEST %s\n", failures ? "failed" : "ok");
    return failures ? 1 : 0;
}

/* MCA_MISC0 of BANK on every CPU that sees a counter there: SMM's
 * thresholding counter (Locked, IntType SMI) and its count. */
static int misc(unsigned bank)
{
    for (unsigned cpu = 0; cpu < 1024; ++cpu) {
        int fd = msr_open(cpu, O_RDONLY);
        if (fd < 0)
            continue;
        uint64_t value = 0, status = 0;
        rdmsr(fd, 0xc0002003 + 16 * bank, &value);
        rdmsr(fd, 0xc0002001 + 16 * bank, &status);
        close(fd);
        if (value >> 62 & 1)
            printf("RAS MISC0 cpu=%u bank=%u misc0=%016" PRIx64 " locked=%u int_type=%u errcnt=%03x overflow=%u status=%016" PRIx64 "\n",
                   cpu, bank, value, (unsigned)(value >> 61 & 1), (unsigned)(value >> 49 & 3),
                   (unsigned)(value >> 32 & 0xfff), (unsigned)(value >> 48 & 1), status);
    }
    return 0;
}

static uint64_t number(const char *s)
{
    return strtoull(s, NULL, 0);
}

int main(int argc, char **argv)
{
    setbuf(stdout, NULL);
    const char *command = argc > 1 ? argv[1] : "";
    int result = 2;
    if (!strcmp(command, "info"))
        result = info();
    else if (!strcmp(command, "selftest"))
        result = selftest();
    else if (!strcmp(command, "smm"))
        result = smm_check();
    else if (!strcmp(command, "probe"))
        result = probe();
    else if (!strcmp(command, "einj") && argc >= 4)
        result = einj(number(argv[2]), number(argv[3]), argc > 4 ? number(argv[4]) : 1);
    else if (!strcmp(command, "einj-pcie") && argc == 4)
        result = einj_pcie(number(argv[2]), argv[3]);
    else if (!strcmp(command, "aer") && argc == 3) {
        unsigned seg, bus, dev, fn;
        result = sscanf(argv[2], "%x:%x:%x.%x", &seg, &bus, &dev, &fn) != 4;
        if (!result)
            aer_status(seg, bus, dev, fn, "now");
    } else if (!strcmp(command, "sp3-einj"))
        result = sp3_einj();
    else if (!strcmp(command, "plant") && argc >= 5)
        result = plant(number(argv[2]), number(argv[3]), number(argv[4]), argc > 5 ? number(argv[5]) : 0, 0);
    else if (!strcmp(command, "plant-reset") && argc >= 5)
        result = plant(number(argv[2]), number(argv[3]), number(argv[4]), argc > 5 ? number(argv[5]) : 0, 1);
    else if (!strcmp(command, "plant-check"))
        result = plant_check();
    else if (!strcmp(command, "mark-ue") && (argc == 3 || argc == 4))
        result = mark_ue(number(argv[2]), argc == 4, argc == 4 ? number(argv[3]) : 0);
    else if (!strcmp(command, "ue-check"))
        result = ue_check();
    else if (!strcmp(command, "espi"))
        result = espi();
    else if (!strcmp(command, "kcs") && argc == 4)
        result = kcs(number(argv[2]), number(argv[3]));
    else if (!strcmp(command, "sel") && argc == 4)
        result = sel(number(argv[2]), number(argv[3]));
    else if (!strcmp(command, "event") && argc == 4)
        result = event(number(argv[2]), number(argv[3]));
    else if (!strcmp(command, "sel-info") && argc == 4)
        result = sel_info(number(argv[2]), number(argv[3]));
    else if (!strcmp(command, "misc") && argc == 3)
        result = misc(number(argv[2]));
    else
        fprintf(stderr, "usage: see the head of tools/hwtest/amd-ras.c\n");
    printf("RAS %s %s\n", command, result ? "FAILED" : "PASSED");
    return result;
}
