use core::{ops::Range, ptr};

use crate::cpu::vmsa::TranslationTable;

const SECTION_SIZE: u32 = 1 << 20;
const SECTION_MASK: u32 = !0 << 20;

/// Reads data from the given address into a buffer.
///
/// Returns how many bytes were possible to read.
pub fn read_memory(start_addr: u32, data: &mut [u8]) -> usize {
    let end_addr = start_addr.saturating_add(data.len() as u32);
    let readable_bytes = test_access(start_addr..end_addr, false);
    log::debug!("Num readable bytes = {readable_bytes}");

    let ptr = start_addr as *const u8;
    // SAFETY: We have ensured these sections are readable before accessing them.
    unsafe {
        ptr::copy(ptr, data.as_mut_ptr(), readable_bytes);
    }

    readable_bytes
}

/// Writes data from a buffer to a given address.
///
/// Returns whether any bytes were written.
pub fn write_memory(start_addr: u32, data: &[u8]) -> bool {
    let end_addr = start_addr.saturating_add(data.len() as u32);
    let writable_bytes = test_access(start_addr..end_addr, true);
    log::debug!("Num writable bytes = {writable_bytes}");

    if writable_bytes < data.len() {
        return false;
    }

    let ptr = start_addr as *mut u8;
    // SAFETY: We have ensured these sections are writable before accessing them.
    unsafe {
        ptr::copy(data.as_ptr(), ptr, writable_bytes);
    }

    true
}

/// Given a range of addresses, returns how many bytes can be written to or read from without
/// faulting.
fn test_access(mut range: Range<u32>, write: bool) -> usize {
    log::debug!("Testing access for addr range {range:?} (write={write})");

    let mut check_addr = range.start & SECTION_MASK;
    while check_addr < range.end {
        let tt = TranslationTable::for_addr(check_addr);
        // SAFETY: This address should be valid for reads because it was sourced from the
        // currently-active translation table.
        let descriptor = unsafe { *tt.lookup_l1(check_addr) };

        // Page-level checks aren't currently implemented because VEXos doesn't seem to use them.
        let can_access = descriptor
            .as_section()
            .and_then(|section| section.access_permissions().ok())
            .map(|perms| {
                if write {
                    perms.test_write(true)
                } else {
                    perms.test_read(true)
                }
            })
            .unwrap_or(false);

        if !can_access {
            range.end = check_addr;
            break;
        }

        check_addr = check_addr.saturating_add(SECTION_SIZE);
    }

    range.end.saturating_sub(range.start) as usize
}
