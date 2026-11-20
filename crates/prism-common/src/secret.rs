//! Keeping passwords out of places they can be read back from.

/// Overwrite the whole allocation of `s` (including spare capacity, where earlier, longer
/// contents may linger) with zeros, then empty it. Volatile writes, so the compiler cannot drop
/// them as dead stores before the memory is freed.
pub fn wipe(s: &mut String) {
    // SAFETY: zero bytes are valid UTF-8, and only bytes inside the allocation are written.
    unsafe {
        let v = s.as_mut_vec();
        let p = v.as_mut_ptr();
        for i in 0..v.capacity() {
            std::ptr::write_volatile(p.add(i), 0);
        }
        v.set_len(0);
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// `String::pop` that also zeros the bytes of the removed character.
pub fn pop(s: &mut String) -> Option<char> {
    let c = s.pop()?;
    // SAFETY: the bytes just past the new length are still inside the allocation.
    unsafe {
        let v = s.as_mut_vec();
        let p = v.as_mut_ptr().add(v.len());
        for i in 0..c.len_utf8() {
            std::ptr::write_volatile(p.add(i), 0);
        }
    }
    Some(c)
}

/// Wipe every string in `v`.
pub fn wipe_all(v: &mut [String]) {
    v.iter_mut().for_each(wipe);
}

/// For processes that handle passwords (lock screen, polkit and keyring prompts): no core dumps,
/// and no ptrace or /proc/<pid>/mem access by other processes of the same user.
pub fn harden_process() {
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wipe_clears_spare_capacity() {
        let mut s = String::from("hunter2-long-password");
        s.truncate(3);
        let (p, cap) = (s.as_ptr(), s.capacity());
        super::wipe(&mut s);
        assert!(s.is_empty());
        let bytes = unsafe { std::slice::from_raw_parts(p, cap) };
        assert!(bytes.iter().all(|&b| b == 0));
    }
}
