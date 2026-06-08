use super::adapter_source_for_board;

// ===== install_logger adapter tests ===============================

#[test]
fn install_logger_emits_arm_per_console_device() {
    // qemu-riscv64 has one Console-providing device: uart0 (ns16550).
    // The match must carry an arm that inits the logger against
    // `.uart0` and returns metadata with both the RON device name and the
    // driver crate name. Runtime owns the console_ready banner.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("fstart_log::init"),
        "install_logger body must call fstart_log::init, got:\n{src}"
    );
    // prettyplease may break `self.uart0` across lines — `.uart0`
    // is the reliable indicator.  Every Console arm references its
    // `self.<field>` to pass to `fstart_log::init`.
    assert!(
        src.contains(".uart0"),
        "install_logger arm must reference self.uart0, got:\n{src}"
    );
    assert!(
        !src.contains("fstart_capabilities::console_ready"),
        "console_ready belongs to fstart-stage-runtime, got:\n{src}"
    );
    assert!(
        src.contains("ConsoleReady"),
        "install_logger body must return ConsoleReady metadata, got:\n{src}"
    );
    assert!(
        src.contains("\"uart0\""),
        "ConsoleReady must carry the RON device name, got:\n{src}"
    );
    assert!(
        src.contains("\"ns16550\""),
        "ConsoleReady must carry the driver crate name, got:\n{src}"
    );
}

#[test]
fn install_logger_pl011_on_aarch64() {
    // qemu-aarch64 uses a Pl011 driver.  The driver-name literal in the
    // returned ConsoleReady metadata must reflect that.
    let src = adapter_source_for_board("qemu-aarch64");
    assert!(src.contains("fstart_log::init"));
    assert!(
        src.contains("\"pl011\""),
        "ConsoleReady must carry \"pl011\" for qemu-aarch64, got:\n{src}"
    );
}

#[test]
fn install_logger_always_has_wildcard_error() {
    // Every generated install_logger body ends with a `_ =>` arm
    // that returns an error.  The executor guarantees the id matches a
    // Console provider, but the compiler still needs exhaustive
    // coverage of the match.
    let src = adapter_source_for_board("qemu-riscv64");
    // Find the `unsafe fn install_logger` signature.  Walk
    // forward matching braces on `{` / `}` to isolate the
    // method body so we don't bleed into the next method.
    let sig_idx = src
        .find("unsafe fn install_logger(")
        .expect("adapter must define install_logger");
    let open = sig_idx
        + src[sig_idx..]
            .find('{')
            .expect("install_logger method must have a body");
    let mut depth = 0i32;
    let mut end = open;
    for (off, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + off + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &src[open..end];
    assert!(
        body.contains("_ =>") && body.contains("RuntimeError::UnknownDevice"),
        "install_logger must include `_ => Err(UnknownDevice)` wildcard, got:\n{body}"
    );
}
