//! QEMU q35 SMM policy.
//!
//! The emulated ICH9 southbridge matches the ICH8 PMBASE layout (64-bit
//! GPE0 at `PMBASE + 0x20`), so the plain ICH8 handler (APMC/ACPI
//! enable, TCO, GPE clear) covers it. This module only binds the board to
//! that handler.

use fstart_driver_intel::ich8::smm::Ich8SmmHandler;
use fstart_platform_qemu::smm::SMM_PLATFORM_INTEL_ICH;

use crate::Board;

fstart_platform_qemu::smm::smm_bin!(Board, SMM_PLATFORM_INTEL_ICH, Ich8SmmHandler);
