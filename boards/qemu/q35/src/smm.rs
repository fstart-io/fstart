//! QEMU q35 SMM policy.
//!
//! The emulated ICH9 southbridge uses the ICH PM I/O layout, so the shared
//! ICH handler (APMC/ACPI enable, TCO, GPE clear) covers it; PMBASE and the
//! GPE0 block come from the installer's runtime data. This module only binds
//! the board to that handler.

use fstart_driver_intel::southbridge::smi::IchSmmHandler;
use fstart_platform_qemu::smm::SMM_PLATFORM_INTEL_ICH;

use crate::Board;

fstart_platform_qemu::smm::smm_bin!(Board, SMM_PLATFORM_INTEL_ICH, IchSmmHandler);
