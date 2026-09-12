//! QEMU DT memory visibility, independent of firmware entry and allocation.

/// A secure-status property overrides status only for secure firmware. The
/// returned flag distinguishes secure-only RAM below the normal RAM ABI base.
pub(crate) fn memory_visibility(
    status: Option<&[u8]>,
    secure_status: Option<&[u8]>,
    secure: bool,
) -> Option<bool> {
    let available = |status: Option<&[u8]>| status.is_none_or(|s| s == b"okay\0" || s == b"ok\0");
    let effective = if secure {
        secure_status.or(status)
    } else {
        status
    };
    available(effective).then(|| !available(status))
}

#[cfg(test)]
mod tests {
    use super::memory_visibility;

    #[test]
    fn secure_memory_requires_secure_execution_and_explicit_availability() {
        let okay: Option<&[u8]> = Some(b"okay\0");
        let disabled: Option<&[u8]> = Some(b"disabled\0");
        for (status, secure_status, secure, expected) in [
            (None, None, false, Some(false)),
            (okay, None, true, Some(false)),
            (disabled, okay, false, None),
            (disabled, okay, true, Some(true)),
            (disabled, None, true, None),
            (disabled, disabled, true, None),
            (okay, disabled, true, None),
            (okay, disabled, false, Some(false)),
        ] {
            assert_eq!(memory_visibility(status, secure_status, secure), expected);
        }
    }
}
