use parking_lot::Mutex;
use std::mem::MaybeUninit;

// this originally happen to be in TLS but there is a limit of TLS
// + the storage itself is not free, so now we rely on the fact that most calls
// are sequential in practice and allocate ONLY when we have a parallel access
static SORT_BUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn ensure_capacity(buf: &mut Vec<u8>, required: usize) {
    if buf.capacity() < required {
        let len = buf.len();
        buf.reserve(required - len);
    }
}

struct SharedSortBuf {
    guard: parking_lot::MutexGuard<'static, Vec<u8>>,
}

impl SharedSortBuf {
    fn as_slice_mut<T>(&mut self, len: usize) -> &mut [MaybeUninit<T>] {
        let align = std::mem::align_of::<MaybeUninit<T>>();
        let size = std::mem::size_of::<MaybeUninit<T>>();
        let required = len.saturating_mul(size).saturating_add(align);
        ensure_capacity(&mut self.guard, required);

        // SAFETY: the Vec<u8> is only 1-byte aligned, so we over-allocate by
        // `align` bytes and shift the pointer to satisfy T's alignment.
        // Callers never read uninitialised data through the returned slice.
        unsafe {
            let ptr = self.guard.as_mut_ptr();
            let offset = ptr.align_offset(align);
            debug_assert!(offset != usize::MAX && offset + len * size <= self.guard.capacity());
            std::slice::from_raw_parts_mut(ptr.add(offset) as *mut MaybeUninit<T>, len)
        }
    }
}

fn try_lock_shared_buf() -> Option<SharedSortBuf> {
    SORT_BUFFER.try_lock().map(|guard| SharedSortBuf { guard })
}

pub fn sort_with_buffer<T, F>(slice: &mut [T], compare: F)
where
    F: FnMut(&T, &T) -> std::cmp::Ordering,
{
    match try_lock_shared_buf() {
        Some(mut buf) => {
            let typed = buf.as_slice_mut::<T>(slice.len());
            glidesort::sort_with_buffer_by(slice, typed, compare);
        }
        None => glidesort::sort_by(slice, compare),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sort_with_buffer() {
        let mut data = vec![5, 2, 8, 1, 9];
        sort_with_buffer(&mut data, |a, b| a.cmp(b));
        assert_eq!(data, vec![1, 2, 5, 8, 9]);
    }

    #[test]
    fn test_reverse_sort() {
        let mut data = vec![1, 2, 3, 4, 5];
        sort_with_buffer(&mut data, |a, b| b.cmp(a));
        assert_eq!(data, vec![5, 4, 3, 2, 1]);
    }

    #[test]
    fn test_empty_slice() {
        let mut data: Vec<i32> = vec![];
        sort_with_buffer(&mut data, |a, b| a.cmp(b));
        assert_eq!(data, Vec::<i32>::new());
    }

    #[test]
    fn test_single_element() {
        let mut data = vec![42];
        sort_with_buffer(&mut data, |a, b| a.cmp(b));
        assert_eq!(data, vec![42]);
    }

    #[test]
    fn test_with_duplicates() {
        let mut data = vec![3, 1, 4, 1, 5, 9, 2, 6, 5];
        sort_with_buffer(&mut data, |a, b| a.cmp(b));
        assert_eq!(data, vec![1, 1, 2, 3, 4, 5, 5, 6, 9]);
    }

    #[test]
    fn test_descending_order() {
        let mut data = vec![3, 1, 4, 1, 5, 9, 2, 6, 5];
        sort_with_buffer(&mut data, |a, b| b.cmp(a));
        assert_eq!(data, vec![9, 6, 5, 5, 4, 3, 2, 1, 1]);
    }

    #[test]
    fn test_simple_descending() {
        let mut data = vec![100, 300, 200];
        sort_with_buffer(&mut data, |a, b| b.cmp(a));
        assert_eq!(data[0], 300);
        assert_eq!(data[1], 200);
        assert_eq!(data[2], 100);
    }
}
