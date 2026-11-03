//! Use the shared ICH7 SMM policy; there are no board-specific trap sources.
fstart_platform_intel::smm::smm_bin!(handler: fstart_driver_intel::southbridge::smi::handle);
