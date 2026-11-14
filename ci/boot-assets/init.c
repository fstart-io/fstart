// The init of the Arm and RISC-V boot-test guests: proves the kernel reached
// userspace by writing FSTART_CI_BOOT_SUCCESS to the console, then idles. It
// needs no libc, so a bare cross compiler builds it.
#define O_WRONLY 1
#define AT_FDCWD -100

#if defined(__riscv) || defined(__aarch64__)
#define SYS_WRITE 64
#define SYS_OPENAT 56
#define SYS_CLOSE 57
#elif defined(__arm__)
#define SYS_WRITE 4
#define SYS_OPENAT 322
#define SYS_CLOSE 6
#else
#error unsupported architecture
#endif

static long syscall1(long nr, long a0) {
#if defined(__riscv)
  register long r0 asm("a0") = a0;
  register long r7 asm("a7") = nr;
  asm volatile("ecall" : "+r"(r0) : "r"(r7) : "memory");
  return r0;
#elif defined(__aarch64__)
  register long r0 asm("x0") = a0;
  register long r8 asm("x8") = nr;
  asm volatile("svc #0" : "+r"(r0) : "r"(r8) : "memory");
  return r0;
#elif defined(__arm__)
  register long r0 asm("r0") = a0;
  register long r7 asm("r7") = nr;
  asm volatile("svc #0" : "+r"(r0) : "r"(r7) : "memory");
  return r0;
#endif
}

static long syscall3(long nr, long a0, long a1, long a2) {
#if defined(__riscv)
  register long r0 asm("a0") = a0;
  register long r1 asm("a1") = a1;
  register long r2 asm("a2") = a2;
  register long r7 asm("a7") = nr;
  asm volatile("ecall" : "+r"(r0) : "r"(r1), "r"(r2), "r"(r7) : "memory");
  return r0;
#elif defined(__aarch64__)
  register long r0 asm("x0") = a0;
  register long r1 asm("x1") = a1;
  register long r2 asm("x2") = a2;
  register long r8 asm("x8") = nr;
  asm volatile("svc #0" : "+r"(r0) : "r"(r1), "r"(r2), "r"(r8) : "memory");
  return r0;
#elif defined(__arm__)
  register long r0 asm("r0") = a0;
  register long r1 asm("r1") = a1;
  register long r2 asm("r2") = a2;
  register long r7 asm("r7") = nr;
  asm volatile("svc #0" : "+r"(r0) : "r"(r1), "r"(r2), "r"(r7) : "memory");
  return r0;
#endif
}

static long syscall4(long nr, long a0, long a1, long a2, long a3) {
#if defined(__riscv)
  register long r0 asm("a0") = a0;
  register long r1 asm("a1") = a1;
  register long r2 asm("a2") = a2;
  register long r3 asm("a3") = a3;
  register long r7 asm("a7") = nr;
  asm volatile("ecall" : "+r"(r0) : "r"(r1), "r"(r2), "r"(r3), "r"(r7) : "memory");
  return r0;
#elif defined(__aarch64__)
  register long r0 asm("x0") = a0;
  register long r1 asm("x1") = a1;
  register long r2 asm("x2") = a2;
  register long r3 asm("x3") = a3;
  register long r8 asm("x8") = nr;
  asm volatile("svc #0" : "+r"(r0) : "r"(r1), "r"(r2), "r"(r3), "r"(r8) : "memory");
  return r0;
#elif defined(__arm__)
  register long r0 asm("r0") = a0;
  register long r1 asm("r1") = a1;
  register long r2 asm("r2") = a2;
  register long r3 asm("r3") = a3;
  register long r7 asm("r7") = nr;
  asm volatile("svc #0" : "+r"(r0) : "r"(r1), "r"(r2), "r"(r3), "r"(r7) : "memory");
  return r0;
#endif
}

static void write_fd(long fd) {
  static const char marker[] = "FSTART_CI_BOOT_SUCCESS\n";
  syscall3(SYS_WRITE, fd, (long)marker, sizeof(marker) - 1);
}

static void marker_to(const char *path) {
  long fd = syscall4(SYS_OPENAT, AT_FDCWD, (long)path, O_WRONLY, 0);
  if (fd >= 0) {
    write_fd(fd);
    syscall1(SYS_CLOSE, fd);
  }
}

void _start(void) {
  write_fd(0);
  write_fd(1);
  write_fd(2);
  marker_to("/dev/console");
  marker_to("/dev/kmsg");
  for (;;) {
    asm volatile("" ::: "memory");
  }
}
