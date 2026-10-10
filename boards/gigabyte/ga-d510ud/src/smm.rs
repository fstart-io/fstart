//! Use the shared ICH7 SMM policy; there are no board-specific trap sources.
use crate::Board;
use fstart_driver_intel::southbridge::smi::IchSmmHandler;
fstart_platform_intel::smm::smm_bin!(Board, IchSmmHandler);
