//! The S8030 reset image: the SP3 platform's flow with this board.
#![no_std]
#![no_main]

// Both crates must be linked for the reset entry (asm/reset.S lives in the
// platform crate) and the board's statics.
use fstart_board_tyan_s8030 as _;
use fstart_platform_sp3 as _;

fstart_platform_sp3::stage_bin!(fstart_board_tyan_s8030::Board);
