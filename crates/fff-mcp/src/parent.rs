#[cfg(unix)]
mod imp {
    pub struct ParentWatcher {
        ppid: u32,
    }

    impl ParentWatcher {
        pub fn new() -> Option<Self> {
            let ppid = std::os::unix::process::parent_id();
            // ppid <= 1 means we were spawned by init and can't detect death
            (ppid > 1).then_some(Self { ppid })
        }

        pub fn parent_pid(&self) -> u32 {
            self.ppid
        }

        pub fn age(&self) -> Option<std::time::Duration> {
            let age = process_age(self.ppid)?;
            self.parent_alive().then_some(age)
        }

        // When the parent dies the kernel reparents us, so getppid() changes.
        // Race-free and immune to PID reuse, unlike kill(ppid, 0).
        pub fn parent_alive(&self) -> bool {
            std::os::unix::process::parent_id() == self.ppid
        }
    }

    #[cfg(target_os = "linux")]
    fn process_age(pid: u32) -> Option<std::time::Duration> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // comm may contain spaces and parentheses; starttime is field 22.
        let ticks: u64 = stat
            .rsplit_once(") ")?
            .1
            .split_whitespace()
            .nth(19)?
            .parse()
            .ok()?;
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks_per_second <= 0 {
            return None;
        }
        let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
        let uptime: f64 = uptime.split_whitespace().next()?.parse().ok()?;
        std::time::Duration::try_from_secs_f64(uptime - ticks as f64 / ticks_per_second as f64).ok()
    }

    #[cfg(target_os = "macos")]
    fn process_age(pid: u32) -> Option<std::time::Duration> {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if read != size {
            return None;
        }
        let info = unsafe { info.assume_init() };
        let started = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(info.pbi_start_tvsec)
            + std::time::Duration::from_micros(info.pbi_start_tvusec);
        std::time::SystemTime::now().duration_since(started).ok()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn process_age(_pid: u32) -> Option<std::time::Duration> {
        None
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{
        CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32, Process32First, Process32Next, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    pub struct ParentWatcher {
        handle: HANDLE,
        ppid: u32,
    }

    // HANDLE is a raw pointer; it is only ever used via WaitForSingleObject
    // which is thread-safe, so moving/sharing the watcher across threads is fine.
    unsafe impl Send for ParentWatcher {}
    unsafe impl Sync for ParentWatcher {}

    impl ParentWatcher {
        pub fn new() -> Option<Self> {
            let ppid = parent_pid_of_current()?;
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, ppid) };
            if handle.is_null() {
                return None;
            }
            // Holding the handle pins the PID, preventing reuse for the process lifetime
            Some(Self { handle, ppid })
        }

        pub fn parent_pid(&self) -> u32 {
            self.ppid
        }

        pub fn age(&self) -> Option<std::time::Duration> {
            // Keep age-query permissions separate from the lifecycle handle.
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, self.ppid) };
            if handle.is_null() {
                return None;
            }
            let mut created: FILETIME = unsafe { std::mem::zeroed() };
            let mut exited = created;
            let mut kernel = created;
            let mut user = created;
            let success = unsafe {
                GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user)
            };
            unsafe { CloseHandle(handle) };
            if success == 0 || !self.parent_alive() {
                return None;
            }
            let ticks =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            let ticks = ticks.checked_sub(116_444_736_000_000_000)?;
            let started = std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(ticks / 10_000_000)
                + std::time::Duration::from_nanos(ticks % 10_000_000 * 100);
            std::time::SystemTime::now().duration_since(started).ok()
        }

        pub fn parent_alive(&self) -> bool {
            unsafe { WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT }
        }
    }

    impl Drop for ParentWatcher {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.handle) };
        }
    }

    fn parent_pid_of_current() -> Option<u32> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut entry: PROCESSENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
            let current = GetCurrentProcessId();
            let mut found = None;
            if Process32First(snapshot, &mut entry) != 0 {
                loop {
                    if entry.th32ProcessID == current {
                        found = Some(entry.th32ParentProcessID);
                        break;
                    }
                    if Process32Next(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snapshot);
            found
        }
    }
}

pub use imp::ParentWatcher;
