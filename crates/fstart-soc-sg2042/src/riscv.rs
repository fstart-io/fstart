//! SG2042 RISC-V subsystem core release driver.
//!
//! Loads the RISC-V ZSBL from SPI flash (via the DMMR memory-mapped
//! window), programs the reset vector into TOP registers, enables all
//! cluster subsystems, and parks the A53 in a WFI loop.
//!
//! Hardware reference: `mango_misc.c` — `mango_setup_rpsys()`,
//! `mango_load_rvfw()`, `mango_load_zsbl()`.
