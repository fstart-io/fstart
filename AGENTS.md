# AGENTS.md — fstart firmware framework

## Code I don't like
- I don't like you reinventing helpers to access for example u32 in a .rs file. There is usually a helper for MMIO already
- Quick solutions in the wrong dir. So we have drivers, platforms and mainboards. Think deeply about the reusability of the scope. See what is already out there as example first
- Reinventing the wheel. I saw agents reinvent PCI ECAM in like 5 different locations or stupid traits that wrap around the original ECAM functions. If you need these boilerplate wrappers your code is wrong!

## Register formatting
Formatting a `register_bitfields!` block is not a hand job: run `cargo ffmt --fix` (`tools/ffmt`), which puts one field and one enum variant per line. CI runs `cargo ffmt --check`.
What it cannot know is the comment: name each register's offset and width (or its SPD byte) in a doc comment.

## Code I do like
- Prefer an existing PAC (Peripheral Access Crate) and/or HAL (Hardware Abstraction Layer) for the hardware over defining registers or drivers ourselves. Reuse these directly rather than rewriting them with tock-registers or adding boilerplate wrappers.
- When no suitable PAC or HAL is available, use tock-registers to define registers and per-register bitfields.
  This is not only for MMIO registers. For mailboxes or staged offset + data writes (common in Super I/O), implement the tock-registers access traits so the same register definitions and bitfields can be used.
