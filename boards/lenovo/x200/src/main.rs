#![no_std]
#![no_main]

#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
fstart_platform_intel::stage_bin!(fstart_board_lenovo_x200::Board);

#[cfg(fstart_stage_env = "smm")]
use fstart_board_lenovo_x200 as _;
