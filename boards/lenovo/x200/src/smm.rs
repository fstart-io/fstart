//! Lenovo ThinkPad X200 SMM: the ICH's own SMI sources only. The EC's events
//! reach the OS as SCIs and the dock is controlled through EC/GPIO, not SMM.

fstart_platform_intel::smm::smm_bin!(handler: fstart_driver_intel::southbridge::smi::handle);
