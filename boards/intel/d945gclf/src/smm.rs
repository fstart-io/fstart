//! Intel D945GCLF mainboard SMM policy.
//!
//! Unlike the X61 dock board, the D945GCLF has no board-specific SMI
//! sources: the plain ICH7 handler (APMC/ACPI enable, TCO, GPE clear)
//! covers it. This module only binds the board to that handler.

use fstart_driver_intel::ich7::smm::Ich7SmmHandler;
use fstart_smm::SMM_PLATFORM_INTEL_ICH;

use crate::Board;

fstart_smm::smm_bin!(Board, SMM_PLATFORM_INTEL_ICH, Ich7SmmHandler);
