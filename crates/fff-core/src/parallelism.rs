use std::sync::LazyLock;

/// Dedicated thread pool for background work (scan, warmup, bigram build).
pub static BACKGROUND_THREAD_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    let total = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4);

    // Background work is mostly syscall-bound; halving parallelism leaves
    // cores for search/UI at negligible throughput cost.
    let bg_threads = (total / 2).max(2);
    rayon::ThreadPoolBuilder::new()
        .num_threads(bg_threads)
        .thread_name(|i| format!("fff-bg-{i}"))
        .start_handler(|_| {
            // Request user-initiated scheduling priority.
            #[cfg(target_os = "macos")]
            unsafe {
                let _ = libc::pthread_set_qos_class_self_np(
                    libc::qos_class_t::QOS_CLASS_USER_INITIATED,
                    0,
                );
            }
        })
        .build()
        .expect("failed to create background rayon pool")
});

/// Grep pool sized to non-efficiency cores on macOS and full parallelism elsewhere.
pub static SEARCH_THREAD_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    #[cfg(target_os = "macos")]
    let threads = non_efficiency_core_count();
    #[cfg(not(target_os = "macos"))]
    let threads = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4);

    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("fff-search-{i}"))
        .start_handler(|_| {
            #[cfg(target_os = "macos")]
            unsafe {
                let _ = libc::pthread_set_qos_class_self_np(
                    libc::qos_class_t::QOS_CLASS_USER_INITIATED,
                    0,
                );
            }
        })
        .build()
        .expect("failed to create search rayon pool")
});

#[cfg(target_os = "macos")]
fn non_efficiency_core_count() -> usize {
    let detected = sysctl_count(c"hw.nperflevels").and_then(|levels| {
        count_non_efficiency_cores((0..levels).map(|level| {
            let name = std::ffi::CString::new(format!("hw.perflevel{level}.name")).ok()?;
            let count = std::ffi::CString::new(format!("hw.perflevel{level}.physicalcpu")).ok()?;
            Some((sysctl_name(&name)?, sysctl_count(&count)?))
        }))
    });

    // Unknown topology falls back to all logical cores, never to a single thread.
    detected
        .or_else(|| sysctl_count(c"hw.perflevel0.physicalcpu").filter(|&count| count > 0))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(4)
        })
}

#[cfg(any(target_os = "macos", test))]
fn count_non_efficiency_cores(
    levels: impl IntoIterator<Item = Option<(String, usize)>>,
) -> Option<usize> {
    let count = levels.into_iter().try_fold(0usize, |total, level| {
        let (name, count) = level?;
        if name.eq_ignore_ascii_case("Efficiency") {
            Some(total)
        } else if ["Super", "Performance", "Standard"]
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
        {
            total.checked_add(count)
        } else {
            None
        }
    })?;
    (count > 0).then_some(count)
}

#[cfg(target_os = "macos")]
fn sysctl_count(name: &std::ffi::CStr) -> Option<usize> {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of_val(&value);
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut value as *mut _ as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size != std::mem::size_of_val(&value) {
        return None;
    }
    usize::try_from(value).ok()
}

#[cfg(target_os = "macos")]
fn sysctl_name(name: &std::ffi::CStr) -> Option<String> {
    let mut value = [0u8; 64];
    let mut size = value.len();
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size > value.len() {
        return None;
    }
    std::ffi::CStr::from_bytes_with_nul(&value[..size])
        .ok()?
        .to_str()
        .ok()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::count_non_efficiency_cores;

    #[test]
    fn includes_super_and_performance_cores() {
        assert_eq!(count(&[("Super", 6), ("Performance", 12)]), Some(18));
        assert_eq!(count(&[("Performance", 12), ("Super", 6)]), Some(18));
    }

    #[test]
    fn excludes_efficiency_cores_in_every_position() {
        assert_eq!(count(&[("Performance", 12), ("Efficiency", 4)]), Some(12));
        assert_eq!(count(&[("Efficiency", 4), ("Performance", 4)]), Some(4));
        assert_eq!(
            count(&[("Super", 6), ("Efficiency", 4), ("Performance", 12)]),
            Some(18)
        );
        assert_eq!(count(&[("Performance", 8), ("efficiency", 4)]), Some(8));
    }

    #[test]
    fn accepts_standard_cores() {
        assert_eq!(count(&[("Performance", 8)]), Some(8));
        assert_eq!(count(&[("Standard", 8)]), Some(8));
    }

    #[test]
    fn unknown_core_types_fall_back() {
        assert_eq!(count(&[("Faster", 6), ("Performance", 12)]), None);
        assert_eq!(count(&[("Super", 6), ("PowerSaving", 4)]), None);
    }

    #[test]
    fn incomplete_or_unusable_topology_falls_back() {
        assert_eq!(count(&[]), None);
        assert_eq!(count(&[("Efficiency", 4)]), None);
        assert_eq!(count(&[("Performance", 0)]), None);
        assert_eq!(
            count_non_efficiency_cores([Some(("Super".into(), 6)), None]),
            None
        );
        assert_eq!(count(&[("Super", usize::MAX), ("Performance", 1)]), None);
    }

    fn count(levels: &[(&str, usize)]) -> Option<usize> {
        count_non_efficiency_cores(
            levels
                .iter()
                .map(|&(name, count)| Some((name.to_owned(), count))),
        )
    }
}
