//! Standard ICH SMI handling; dock control uses EC/GPIO directly, not SMM.
use crate::Board;
use fstart_driver_intel::southbridge::smi::IchSmmHandler;

fstart_platform_intel::smm::smm_bin!(Board, IchSmmHandler);
