//! Intel D945GCLF mainboard SMM policy.
//!
//! Unlike the X61 dock board, the D945GCLF has no board-specific SMI
//! sources: the plain ICH7 handler (APMC/ACPI enable, TCO, GPE clear)
//! covers it. This module only binds that handler.

fstart_platform_intel::smm::smm_bin!(handler: fstart_driver_intel::southbridge::smi::handle);
