# AGENTS.md — fstart firmware framework

## Code I don't like
- I don't like you reinventing helpers to access for example u32 in a .rs file. There is usually a helper for MMIO already
- Quick solutions in the wrong dir. So we have drivers, platforms and mainboards. Think deeply about the reusability of the scope. See what is already out there as example first
- Reinventing the wheel. I saw agents reinvent PCI ECAM in like 5 different locations or stupid traits that wrap around the original ECAM functions. If you need these boilerplate wrappers your code is wrong!

## Code I do like
- I love Tock-register! Tock-register is a library to program registers and define per register bitfields. We're a hardware project and we should use this uquitously
  This is not only for MMIO registers. If you have some port of a mailbox or staged offset + data writes (common in superio), the tock-register trait "Readable, Writeable, ReadWriteable" allow us to use the same register offset + bitfields for this.
