use std::ops::Deref;
use std::ptr::NonNull;

pub struct ColumnSlab {
    ptr: *mut u64,
    len: usize,
    // unix: anonymous mmap so `truncate` can hand trailing pages back to the OS
    #[cfg(unix)]
    mapped_bytes: usize,
    #[cfg(not(unix))]
    vec: Vec<u64>,
}

unsafe impl Send for ColumnSlab {}
unsafe impl Sync for ColumnSlab {}

impl ColumnSlab {
    pub fn empty() -> Self {
        Self {
            ptr: NonNull::dangling().as_ptr(),
            len: 0,
            #[cfg(unix)]
            mapped_bytes: 0,
            #[cfg(not(unix))]
            vec: Vec::new(),
        }
    }

    /// `None` when the OS refuses the allocation; callers degrade instead of aborting.
    pub fn new(len: usize) -> Option<Self> {
        if len == 0 {
            return Some(Self::empty());
        }
        #[cfg(unix)]
        {
            let mapped_bytes = len.checked_mul(8)?.checked_next_multiple_of(page_size())?;
            // SAFETY: anonymous private mapping
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    mapped_bytes,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return None;
            }
            Some(Self {
                ptr: ptr as *mut u64,
                len,
                mapped_bytes,
            })
        }
        #[cfg(not(unix))]
        {
            let mut vec = Vec::new();
            vec.try_reserve_exact(len).ok()?;
            vec.resize(len, 0);
            Self::from_vec(vec)
        }
    }

    pub fn from_vec(vec: Vec<u64>) -> Option<Self> {
        #[cfg(unix)]
        {
            let mut slab = Self::new(vec.len())?;
            slab.as_mut_slice().copy_from_slice(&vec);
            Some(slab)
        }
        #[cfg(not(unix))]
        {
            let mut vec = vec;
            Some(Self {
                ptr: vec.as_mut_ptr(),
                len: vec.len(),
                vec,
            })
        }
    }

    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut u64 {
        self.ptr
    }

    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u64] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// Shrink to the first `len` words, returning the trailing pages to the OS.
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        self.len = len;
        #[cfg(unix)]
        {
            let keep = if len == 0 {
                0
            } else {
                (len * 8).next_multiple_of(page_size())
            };
            if keep < self.mapped_bytes {
                // SAFETY: unmapping a page-aligned tail of our own mapping.
                unsafe {
                    libc::munmap(
                        (self.ptr as *mut u8).add(keep).cast(),
                        self.mapped_bytes - keep,
                    );
                }
                self.mapped_bytes = keep;
                if keep == 0 {
                    self.ptr = NonNull::dangling().as_ptr();
                }
            }
        }
        #[cfg(not(unix))]
        {
            self.vec.truncate(len);
            self.vec.shrink_to_fit();
            self.ptr = self.vec.as_mut_ptr();
        }
    }
}

impl Deref for ColumnSlab {
    type Target = [u64];

    #[inline]
    fn deref(&self) -> &[u64] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

#[cfg(unix)]
impl Drop for ColumnSlab {
    fn drop(&mut self) {
        if self.mapped_bytes > 0 {
            unsafe {
                libc::munmap(self.ptr.cast(), self.mapped_bytes);
            }
        }
    }
}

impl std::fmt::Debug for ColumnSlab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnSlab")
            .field("words", &self.len)
            .finish()
    }
}

#[cfg(unix)]
fn page_size() -> usize {
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zeroed_truncate_and_roundtrip() {
        let mut slab = ColumnSlab::new(10_000).unwrap();
        assert!(slab.iter().all(|&w| w == 0));
        slab.as_mut_slice()[9_999] = 7;
        slab.as_mut_slice()[3] = 5;
        slab.truncate(4);
        assert_eq!(&slab[..], &[0, 0, 0, 5]);
        slab.truncate(100);
        assert_eq!(slab.len(), 4);
        slab.truncate(0);
        assert!(slab.is_empty());

        let v = ColumnSlab::from_vec(vec![1, 2, 3]).unwrap();
        assert_eq!(&v[..], &[1, 2, 3]);
        let empty = ColumnSlab::new(0).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn unmappable_size_is_none_not_a_panic() {
        assert!(ColumnSlab::new(usize::MAX / 16).is_none());
        assert!(ColumnSlab::new(usize::MAX).is_none());
    }
}
