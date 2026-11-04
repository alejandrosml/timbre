//! Whose permissions macOS checks when Timbre reads Mail's data.
//!
//! macOS checks privacy permissions (Full Disk Access, control of Mail)
//! against the *responsible* process. A program started by Claude Desktop
//! inherits Claude as its responsible process, so the access the user gives
//! to `timbre` in System Settings would be ignored and Claude's would apply.
//!
//! The server therefore starts itself once more with its responsibility
//! disclaimed, the way Chromium and Qt Creator launch other programs. macOS
//! then checks Timbre's own permissions. The first process only waits for the
//! second and passes on its exit code; standard input and output are shared,
//! so Claude talks to the second process directly.

/// Set in the relaunched process. Set it by hand to keep the old behaviour.
#[cfg(target_os = "macos")]
const MARK: &str = "TIMBRE_DISCLAIMED";

/// Starts the server again as a process responsible for itself, waits for it
/// and returns its exit code. Returns `None` when this process should serve:
/// it is the relaunched one, it is already responsible for itself, or macOS
/// does not offer the mechanism.
#[cfg(target_os = "macos")]
pub fn relaunch() -> Option<i32> {
    if std::env::var_os(MARK).is_some() || responsible_for_self() != Some(false) {
        return None;
    }
    match spawn_disclaimed() {
        Ok(pid) => Some(wait(pid)),
        Err(e) => {
            tracing::warn!("could not relaunch with its own permissions, Claude's apply: {e}");
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn relaunch() -> Option<i32> {
    None
}

/// In the relaunched process: exit when the launcher is gone, in case Claude
/// stops it without closing standard input.
#[cfg(target_os = "macos")]
pub fn watch_launcher() {
    if std::env::var_os(MARK).is_none() {
        return;
    }
    let launcher = std::os::unix::process::parent_id();
    let _ = std::thread::Builder::new().name("timbre-launcher".into()).spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if std::os::unix::process::parent_id() != launcher {
                tracing::info!("launcher gone, exiting");
                std::process::exit(0);
            }
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub fn watch_launcher() {}

/// The program whose permissions macOS checks for this process, for status
/// reports. `None` where macOS does not say.
#[cfg(target_os = "macos")]
pub fn checked_for() -> Option<String> {
    let pid = responsible_pid()?;
    if pid == std::process::id() as libc::pid_t {
        return Some("timbre (this program)".into());
    }
    Some(pid_path(pid).unwrap_or_else(|| format!("process {pid}")))
}

#[cfg(not(target_os = "macos"))]
pub fn checked_for() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn responsible_for_self() -> Option<bool> {
    Some(responsible_pid()? == std::process::id() as libc::pid_t)
}

/// Looks up a function of libSystem that Apple does not declare publicly.
/// Looking it up at run time keeps the build independent of the SDK, and an
/// older or newer macOS without it simply keeps the old behaviour.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn symbol(name: &std::ffi::CStr) -> Option<*mut libc::c_void> {
    // SAFETY: dlsym with RTLD_DEFAULT only reads the loaded images' tables.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!p.is_null()).then_some(p)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn responsible_pid() -> Option<libc::pid_t> {
    type ResponsibleFor = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;
    let f = symbol(c"responsibility_get_pid_responsible_for_pid")?;
    // SAFETY: the symbol has this signature since macOS 10.14; it only reads
    // the kernel's record for the given process.
    let pid = unsafe {
        let f: ResponsibleFor = std::mem::transmute::<*mut libc::c_void, ResponsibleFor>(f);
        f(std::process::id() as libc::pid_t)
    };
    (pid > 0).then_some(pid)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn pid_path(pid: libc::pid_t) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is writable and its size is passed along.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn spawn_disclaimed() -> Result<libc::pid_t, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    type SetDisclaim = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, libc::c_int) -> libc::c_int;
    let set =
        symbol(c"responsibility_spawnattrs_setdisclaim").ok_or("macOS has no responsibility_spawnattrs_setdisclaim")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let cstr = |b: &[u8]| CString::new(b).map_err(|_| "NUL byte in an argument or the environment".to_string());
    let path = cstr(exe.as_os_str().as_bytes())?;
    let args = std::env::args_os().map(|a| cstr(a.as_bytes())).collect::<Result<Vec<_>, _>>()?;
    let mut env = std::env::vars_os()
        .filter(|(k, _)| k != MARK)
        .map(|(k, v)| cstr(&[k.as_bytes(), b"=", v.as_bytes()].concat()))
        .collect::<Result<Vec<_>, _>>()?;
    env.push(cstr(format!("{MARK}=1").as_bytes())?);
    let argv: Vec<*mut libc::c_char> =
        args.iter().map(|a| a.as_ptr().cast_mut()).chain(std::iter::once(std::ptr::null_mut())).collect();
    let envp: Vec<*mut libc::c_char> =
        env.iter().map(|e| e.as_ptr().cast_mut()).chain(std::iter::once(std::ptr::null_mut())).collect();

    // SAFETY: every pointer refers to a NUL-terminated string or array that
    // outlives the call; the attributes are initialised before use and
    // destroyed after. Without file actions the child shares our standard
    // input, output and error (every other descriptor is close-on-exec).
    unsafe {
        let set: SetDisclaim = std::mem::transmute::<*mut libc::c_void, SetDisclaim>(set);
        let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
        if libc::posix_spawnattr_init(&mut attr) != 0 {
            return Err("posix_spawnattr_init failed".into());
        }
        let mut pid: libc::pid_t = 0;
        let rc = match set(&mut attr, 1) {
            0 => libc::posix_spawn(&mut pid, path.as_ptr(), std::ptr::null(), &attr, argv.as_ptr(), envp.as_ptr()),
            e => e,
        };
        libc::posix_spawnattr_destroy(&mut attr);
        if rc != 0 {
            return Err(format!("posix_spawn: {}", std::io::Error::from_raw_os_error(rc)));
        }
        Ok(pid)
    }
}

/// Waits for the relaunched server and returns the code to exit with.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn wait(pid: libc::pid_t) -> i32 {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: waits for our own child and writes into a local.
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r == pid {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return 1;
        }
    }
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}
