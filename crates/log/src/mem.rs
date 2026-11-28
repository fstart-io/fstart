//! In-memory ring-buffer console log.
//!
//! Stages write console output into a single buffer that travels with the boot:
//! optionally starting in CAR on platforms with room, moving into the firmware
//! store's `CONSOLE` entry once DRAM is up, and staying attached across
//! postcar, ramstage and UEFI payload execution.
//!
//! A consumer can snapshot the initialized bytes in order without allocating,
//! and detach the sink atomically with the final snapshot at handoff.
//!
//! All buffer access and state transitions are serialized by an internal
//! spinlock (`MEM_LOCK`). Without an attached buffer, every operation is a
//! cheap no-op.

use core::cell::UnsafeCell;
use portable_atomic::{AtomicBool, Ordering};

/// Header size in bytes preceding the ring-buffer body.
pub const HEADER_SIZE: usize = size_of::<Header>();

/// Bit 31 of [`Header::cursor`]: set once the ring buffer has wrapped.
const OVERFLOW: u32 = 1 << 31;

/// Low 31 bits of [`Header::cursor`]: next write offset in `0..size`.
const CURSOR_MASK: u32 = !OVERFLOW;

/// Ring-buffer header; `size` body bytes follow it immediately.
#[derive(Clone, Copy)]
#[repr(C)]
struct Header {
    /// Body capacity in bytes (capped at `CURSOR_MASK`).
    size: u32,
    /// Write offset in `0..size` (low 31 bits) | [`OVERFLOW`] if wrapped.
    cursor: u32,
}

/// Attached backing buffer. Kept as a base `*mut u8` so header and body
/// accesses each derive from full-buffer provenance without creating an
/// intermediate `&Header` / `&mut Header` reference covering only 8 bytes.
#[derive(Clone, Copy)]
struct Slot {
    ptr: *mut u8,
}

struct SlotCell(UnsafeCell<Option<Slot>>);

// SAFETY: Every access to the inner `Option<Slot>` and the memory it points to
// is serialized by `MEM_LOCK`.
unsafe impl Sync for SlotCell {}

static MEM_LOCK: AtomicBool = AtomicBool::new(false);
static SLOT: SlotCell = SlotCell(UnsafeCell::new(None));

struct MemLockGuard;

impl MemLockGuard {
    fn acquire() -> Self {
        while MEM_LOCK
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Self
    }

    fn try_acquire() -> Option<Self> {
        MEM_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for MemLockGuard {
    fn drop(&mut self) {
        MEM_LOCK.store(false, Ordering::Release);
    }
}

fn with_slot<R>(f: impl FnOnce(&mut Option<Slot>) -> R) -> R {
    let _guard = MemLockGuard::acquire();
    // SAFETY: `MEM_LOCK` grants exclusive access to `SLOT`.
    f(unsafe { &mut *SLOT.0.get() })
}

/// Total bytes needed for a ring buffer holding `body_bytes` of log text.
#[must_use]
pub const fn buffer_size(body_bytes: usize) -> usize {
    HEADER_SIZE + body_bytes
}

fn body_capacity(len: usize) -> Option<u32> {
    let body = len.checked_sub(HEADER_SIZE)?;
    if body == 0 {
        return None;
    }
    Some(body.min(CURSOR_MASK as usize) as u32)
}

fn check_buffer_bounds(buffer: *mut u8, len: usize) -> Option<u32> {
    if buffer.is_null() || !(buffer as usize).is_multiple_of(align_of::<Header>()) {
        return None;
    }
    body_capacity(len)
}

/// Read the header from `base`.
///
/// # Safety
/// `base` must be non-null, aligned to `align_of::<Header>()`, and readable
/// for at least `HEADER_SIZE` initialized bytes.
unsafe fn read_header(base: *mut u8) -> Header {
    // SAFETY: caller guarantees alignment and readability of `Header`.
    unsafe { base.cast::<Header>().read() }
}

/// Write `header` to `base`.
///
/// # Safety
/// `base` must be non-null, aligned to `align_of::<Header>()`, and writable
/// for at least `HEADER_SIZE` bytes.
unsafe fn write_header(base: *mut u8, header: Header) {
    // SAFETY: caller guarantees alignment and writability of `Header`.
    unsafe { base.cast::<Header>().write(header) };
}

fn valid_header(header: Header, max_body: u32) -> bool {
    let size = header.size;
    let pos = header.cursor & CURSOR_MASK;
    size > 0 && size <= max_body && pos <= size
}

fn used_len(header: Header) -> usize {
    if header.cursor & OVERFLOW != 0 {
        header.size as usize
    } else {
        (header.cursor & CURSOR_MASK).min(header.size) as usize
    }
}

/// Copy `dst.len()` initialized bytes from the body of `slot` starting at
/// `body_offset` into `dst`.
///
/// # Safety
/// `body_offset + dst.len() <= header.size` and those body bytes must have
/// been initialized.
unsafe fn copy_body_range(slot: Slot, body_offset: usize, dst: &mut [u8]) {
    if dst.is_empty() {
        return;
    }
    // SAFETY: caller guarantees `body_offset..body_offset + dst.len()` lies
    // within the initialized part of the body following the validated header.
    unsafe {
        core::ptr::copy_nonoverlapping(
            slot.ptr.add(HEADER_SIZE + body_offset),
            dst.as_mut_ptr(),
            dst.len(),
        );
    }
}

/// Copy linearized bytes (oldest to newest) from `slot` into `dst`.
/// If `dst` is smaller than the stored log, copies the newest `dst.len()` bytes.
///
/// # Safety
/// `slot` must point to a valid attached ring buffer.
unsafe fn copy_from_slot(slot: Slot, dst: &mut [u8]) -> usize {
    // SAFETY: `slot` holds a validated header.
    let header = unsafe { read_header(slot.ptr) };
    let total = used_len(header);
    if total == 0 || dst.is_empty() {
        return 0;
    }
    let size = header.size as usize;
    let pos = (header.cursor & CURSOR_MASK) as usize;
    let copy_len = total.min(dst.len());
    let skip = total - copy_len;

    if header.cursor & OVERFLOW == 0 {
        // SAFETY: `skip + copy_len <= pos <= size`, all initialized.
        unsafe { copy_body_range(slot, skip, &mut dst[..copy_len]) };
    } else {
        // Logical stream is `body[pos..size]` followed by `body[0..pos]`.
        let first_len = size - pos;
        if skip >= first_len {
            let start = skip - first_len;
            // SAFETY: wrapped buffer has all `0..size` initialized.
            unsafe { copy_body_range(slot, start, &mut dst[..copy_len]) };
        } else {
            let from_first = (first_len - skip).min(copy_len);
            // SAFETY: wrapped buffer has all `0..size` initialized.
            unsafe { copy_body_range(slot, pos + skip, &mut dst[..from_first]) };
            let rem = copy_len - from_first;
            if rem > 0 {
                // SAFETY: wrapped buffer has all `0..size` initialized.
                unsafe { copy_body_range(slot, 0, &mut dst[from_first..copy_len]) };
            }
        }
    }
    copy_len
}

/// Append `bytes` to `slot`, stripping `b'\r'` so Unix `\n` lines stay clean.
///
/// # Safety
/// `slot` must point to a valid attached ring buffer with exclusive access.
unsafe fn append_to_slot(slot: Slot, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // SAFETY: `slot` holds a validated header.
    let mut header = unsafe { read_header(slot.ptr) };
    let size = header.size as usize;
    if size == 0 {
        return;
    }
    let mut overflow = header.cursor & OVERFLOW;
    let mut pos = (header.cursor & CURSOR_MASK) as usize;
    if pos >= size {
        pos = 0;
        overflow = OVERFLOW;
    }
    for &b in bytes {
        if b == b'\r' {
            continue;
        }
        // SAFETY: `pos < size`, validated against the backing buffer on attach; writing via
        // raw pointer from `slot.ptr` preserves provenance and never reads the
        // potentially uninitialized destination byte.
        unsafe { slot.ptr.add(HEADER_SIZE + pos).write(b) };
        pos += 1;
        if pos == size {
            pos = 0;
            overflow = OVERFLOW;
        }
    }
    header.cursor = (pos as u32) | overflow;
    // SAFETY: `slot` is aligned and writable for `HEADER_SIZE` bytes.
    unsafe { write_header(slot.ptr, header) };
}

/// Append the linearized contents of `src` into `dst` byte-by-byte without
/// touching uninitialized bytes in either buffer.
///
/// # Safety
/// `src` and `dst` must be valid, disjoint ring buffers.
unsafe fn append_slot_to_slot(src: Slot, dst: Slot) {
    // SAFETY: `src` is a validated ring buffer.
    let header = unsafe { read_header(src.ptr) };
    let size = header.size as usize;
    let pos = (header.cursor & CURSOR_MASK) as usize;
    let copy_slice = |start: usize, len: usize| {
        if len == 0 {
            return;
        }
        // SAFETY: `start..start + len` is initialized within `src`'s body and
        // disjoint from `dst`.
        let slice = unsafe { core::slice::from_raw_parts(src.ptr.add(HEADER_SIZE + start), len) };
        // SAFETY: `dst` is valid and exclusively locked.
        unsafe { append_to_slot(dst, slice) };
    };
    if header.cursor & OVERFLOW == 0 {
        copy_slice(0, pos.min(size));
    } else {
        copy_slice(pos, size - pos);
        copy_slice(0, pos);
    }
}

/// Initialize an empty memory log in the `len` bytes at `buffer` and attach it.
///
/// # Safety
/// `buffer` must be at least 4-byte aligned and point to `len` writable bytes
/// that remain valid (and not otherwise accessed) while attached.
pub unsafe fn init(buffer: *mut u8, len: usize) -> bool {
    let Some(size) = check_buffer_bounds(buffer, len) else {
        return false;
    };
    with_slot(|slot| {
        // SAFETY: caller provides aligned, writable memory of `len` bytes.
        unsafe { write_header(buffer, Header { size, cursor: 0 }) };
        *slot = Some(Slot { ptr: buffer });
        true
    })
}

/// Attach a memory log left by an earlier stage in the `len` bytes at `buffer`.
///
/// # Safety
/// As for [`init`]; when `len >= HEADER_SIZE`, the first `HEADER_SIZE` bytes
/// must hold an initialized header previously written by [`init`].
pub unsafe fn attach(buffer: *mut u8, len: usize) -> bool {
    let Some(max_body) = check_buffer_bounds(buffer, len) else {
        return false;
    };
    with_slot(|slot| {
        // SAFETY: `check_buffer_bounds` verified alignment and `len > HEADER_SIZE`;
        // `MEM_LOCK` serializes against any writer if `buffer` is already attached.
        let header = unsafe { read_header(buffer) };
        if !valid_header(header, max_body) {
            return false;
        }
        *slot = Some(Slot { ptr: buffer });
        true
    })
}

/// Move the currently attached memory log into the `len` bytes at `buffer` and
/// attach the copy. If no log was attached yet, initializes an empty log at
/// `buffer`.
///
/// # Safety
/// As for [`init`]; `buffer` must not overlap the currently attached buffer.
pub unsafe fn move_to(buffer: *mut u8, len: usize) -> bool {
    let Some(new_size) = check_buffer_bounds(buffer, len) else {
        return false;
    };
    with_slot(|slot| {
        // SAFETY: caller provides aligned, writable, disjoint memory of `len` bytes.
        unsafe {
            write_header(
                buffer,
                Header {
                    size: new_size,
                    cursor: 0,
                },
            );
        }
        let new_slot = Slot { ptr: buffer };
        if let Some(old) = *slot {
            // SAFETY: `old` and `new_slot` are valid and disjoint; `append_slot_to_slot`
            // writes only initialized bytes and wraps cleanly if `new_size` is smaller.
            unsafe { append_slot_to_slot(old, new_slot) };
        }
        *slot = Some(new_slot);
        true
    })
}

/// Append any currently attached (e.g. CAR) log into an existing log at
/// `buffer` and attach `buffer`. Used on S3 resume so the store entry is not
/// resized or wiped.
///
/// # Safety
/// As for [`attach`]; `buffer` must not overlap the currently attached buffer.
pub unsafe fn append_to(buffer: *mut u8, len: usize) -> bool {
    let Some(max_body) = check_buffer_bounds(buffer, len) else {
        return false;
    };
    // SAFETY: `check_buffer_bounds` verified alignment and `len > HEADER_SIZE`.
    let header = unsafe { read_header(buffer) };
    if !valid_header(header, max_body) {
        return false;
    }
    with_slot(|slot| {
        let target = Slot { ptr: buffer };
        if let Some(old) = *slot {
            // SAFETY: `old` and `target` are valid and disjoint.
            unsafe { append_slot_to_slot(old, target) };
        }
        *slot = Some(target);
        true
    })
}

/// Detach the current memory log buffer.
pub fn detach() {
    with_slot(|slot| {
        *slot = None;
    });
}

/// Whether a memory log buffer is currently attached.
#[inline]
pub fn is_attached() -> bool {
    with_slot(|slot| slot.is_some())
}

/// Body capacity of the attached memory log, or 0 if none is attached.
pub fn capacity() -> usize {
    with_slot(|slot| {
        slot.map_or(0, |s| {
            // SAFETY: attached slot holds a validated header.
            unsafe { read_header(s.ptr) }.size as usize
        })
    })
}

/// Number of valid log bytes currently stored.
pub fn len() -> usize {
    with_slot(|slot| {
        slot.map_or(0, |s| {
            // SAFETY: attached slot holds a validated header.
            used_len(unsafe { read_header(s.ptr) })
        })
    })
}

/// Whether the attached log has no bytes stored.
pub fn is_empty() -> bool {
    len() == 0
}

/// Append `bytes` to the attached memory log, returning `true` if a buffer was
/// attached and accepted the bytes.
#[inline]
pub fn write_bytes(bytes: &[u8]) -> bool {
    with_slot(|slot| {
        let Some(s) = *slot else {
            return false;
        };
        if !bytes.is_empty() {
            // SAFETY: `MEM_LOCK` is held and `s` is a validated slot.
            unsafe { append_to_slot(s, bytes) };
        }
        true
    })
}

/// Best-effort append used by the panic handler: skips the memory log if
/// `MEM_LOCK` is already held (e.g. a panic inside a memory-log call) rather
/// than deadlocking or violating exclusivity. Returns `true` if the bytes were
/// written to an attached buffer.
#[inline]
pub(crate) fn try_write_bytes(bytes: &[u8]) -> bool {
    let Some(_guard) = MemLockGuard::try_acquire() else {
        return false;
    };
    // SAFETY: `_guard` holds `MEM_LOCK` exclusively.
    let Some(s) = (unsafe { *SLOT.0.get() }) else {
        return false;
    };
    if !bytes.is_empty() {
        // SAFETY: `s` is a validated slot and we hold `MEM_LOCK`.
        unsafe { append_to_slot(s, bytes) };
    }
    true
}

/// Copy the linearized log (oldest to newest) into `dst`, returning the number
/// of bytes written. If `dst` is smaller than [`len`], copies the newest tail.
pub fn copy_to(dst: &mut [u8]) -> usize {
    with_slot(|slot| snapshot(*slot, dst))
}

/// Copy the final snapshot and detach under the same lock, so subsequent
/// writers cannot touch the backing buffer after handoff.
pub fn copy_to_and_detach(dst: &mut [u8]) -> usize {
    with_slot(|slot| snapshot(slot.take(), dst))
}

fn snapshot(slot: Option<Slot>, dst: &mut [u8]) -> usize {
    slot.map_or(0, |s| {
        // SAFETY: called with MEM_LOCK held and a validated slot.
        unsafe { copy_from_slot(s, dst) }
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::mem::MaybeUninit;
    use std::sync::Mutex;

    // Tests share the global SLOT.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[repr(align(8))]
    struct UninitBuf<const N: usize>([MaybeUninit<u8>; N]);

    impl<const N: usize> UninitBuf<N> {
        const fn uninit() -> Self {
            Self([MaybeUninit::uninit(); N])
        }

        fn as_mut_ptr(&mut self) -> *mut u8 {
            self.0.as_mut_ptr().cast::<u8>()
        }

        const fn len(&self) -> usize {
            N
        }
    }

    #[test]
    fn write_wrap_move_and_copy_on_uninit_backing() {
        let _guard = TEST_LOCK.lock().unwrap();
        detach();

        // Backing memory is intentionally uninitialized (like `Store::add`).
        let mut car = UninitBuf::<{ buffer_size(8) }>::uninit();
        // SAFETY: aligned stack buffer, detached before return.
        assert!(unsafe { init(car.as_mut_ptr(), car.len()) });
        assert_eq!(capacity(), 8);
        assert_eq!(len(), 0);

        write_bytes(b"hello\r\n");
        assert_eq!(len(), 6);
        let mut out = [0u8; 16];
        let n = copy_to(&mut out);
        assert_eq!(&out[..n], b"hello\n");

        // Wrap around the 8-byte ring buffer.
        write_bytes(b"12345");
        assert_eq!(len(), 8);
        let n = copy_to(&mut out);
        assert_eq!(&out[..n], b"lo\n12345");

        // Smaller destination keeps the newest tail.
        let mut tail = [0u8; 4];
        let n = copy_to(&mut tail);
        assert_eq!(&tail[..n], b"2345");

        // Move into a larger uninitialized DRAM buffer and keep appending.
        let mut dram = UninitBuf::<{ buffer_size(32) }>::uninit();
        assert!(unsafe { move_to(dram.as_mut_ptr(), dram.len()) });
        assert_eq!(capacity(), 32);
        write_bytes(b"-end");
        let n = copy_to(&mut out);
        assert_eq!(&out[..n], b"lo\n12345-end");

        // Move into a smaller buffer keeps only the newest tail.
        let mut small = UninitBuf::<{ buffer_size(5) }>::uninit();
        assert!(unsafe { move_to(small.as_mut_ptr(), small.len()) });
        let n = copy_to(&mut out);
        assert_eq!(&out[..n], b"5-end");

        // Re-attach validates bounds.
        assert!(unsafe { attach(dram.as_mut_ptr(), dram.len()) });
        assert!(!unsafe { attach(dram.as_mut_ptr(), buffer_size(16)) });

        detach();
    }

    #[test]
    fn short_or_misaligned_buffers_are_rejected_without_reading_header() {
        let _guard = TEST_LOCK.lock().unwrap();
        detach();

        // Uninitialized short buffers must never be dereferenced.
        let mut tiny = UninitBuf::<8>::uninit();
        for short_len in [0, 1, 4, 7, 8] {
            assert!(!unsafe { init(tiny.as_mut_ptr(), short_len) });
            assert!(!unsafe { attach(tiny.as_mut_ptr(), short_len) });
            assert!(!unsafe { append_to(tiny.as_mut_ptr(), short_len) });
            assert!(!unsafe { move_to(tiny.as_mut_ptr(), short_len) });
        }

        // Misaligned pointer rejected.
        let mut buf = UninitBuf::<32>::uninit();
        let misaligned = unsafe { buf.as_mut_ptr().add(1) };
        assert!(!unsafe { init(misaligned, 24) });
        assert!(!unsafe { attach(misaligned, 24) });
        assert!(!unsafe { append_to(misaligned, 24) });

        detach();
    }

    #[test]
    fn move_from_detached_initializes_fresh_buffer() {
        let _guard = TEST_LOCK.lock().unwrap();
        detach();

        let mut dram = UninitBuf::<{ buffer_size(16) }>::uninit();
        assert!(unsafe { move_to(dram.as_mut_ptr(), dram.len()) });
        assert_eq!(capacity(), 16);
        assert_eq!(len(), 0);
        write_bytes(b"dram-only");
        let mut out = [0u8; 16];
        let n = copy_to(&mut out);
        assert_eq!(&out[..n], b"dram-only");

        detach();
    }

    #[test]
    fn append_to_preserves_existing_log_on_resume() {
        let _guard = TEST_LOCK.lock().unwrap();
        detach();

        let mut dram = UninitBuf::<{ buffer_size(32) }>::uninit();
        assert!(unsafe { init(dram.as_mut_ptr(), dram.len()) });
        write_bytes(b"cold\n");

        let mut car = UninitBuf::<{ buffer_size(16) }>::uninit();
        assert!(unsafe { init(car.as_mut_ptr(), car.len()) });
        write_bytes(b"s3-resume\n");

        assert!(unsafe { append_to(dram.as_mut_ptr(), dram.len()) });
        let mut out = [0u8; 32];
        let n = copy_to(&mut out);
        assert_eq!(&out[..n], b"cold\ns3-resume\n");

        detach();
    }

    #[test]
    fn final_snapshot_detaches_the_sink() {
        let _guard = TEST_LOCK.lock().unwrap();
        detach();

        let mut log_buf = UninitBuf::<{ buffer_size(64) }>::uninit();
        assert!(unsafe { init(log_buf.as_mut_ptr(), log_buf.len()) });
        write_bytes(b"bootblock\n");

        let mut dst = [0; 64];
        let n = copy_to_and_detach(&mut dst);
        assert_eq!(&dst[..n], b"bootblock\n");
        assert!(!write_bytes(b"after handoff"));
        assert!(!is_attached());
    }
}
