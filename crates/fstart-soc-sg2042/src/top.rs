//! SG2042 SYS_CTRL (TOP) register block and clock controller driver.
//!
//! Covers: PLL status, clock gate enables, IP soft-resets, pin mux,
//! I2C-based MCU board detection, and RISC-V subsystem control registers.
//!
//! Hardware reference: `plat/sophgo/mango/include/platform_def.h`,
//! `mango_clock.c`, `mango_common.c`, `mango_bl2_setup.c`.
