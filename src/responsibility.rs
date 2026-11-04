//! Whose permissions macOS checks when Timbre reads Mail's data.
//!
//! macOS checks privacy permissions (Full Disk Access, control of Mail)
//! against the *responsible* process. A program started by Claude Desktop
//! inherits Claude as its responsible process, so the access the user gives
//! to Timbre in System Settings would be ignored and Claude's would apply.
//!
//! The launcher (launcher.rs) therefore starts the server with its
//! responsibility disclaimed, the way Chromium and Qt Creator start other
//! programs: macOS then checks Timbre's own permissions, and only Timbre's.
//! The functions Apple uses for this are not declared in its public headers;
//! they are looked up at run time, and a macOS without them keeps the old
//! behaviour.

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

/// Whether this macOS lets a program start another one responsible for
/// itself.
#[cfg(target_os = "macos")]
pub fn can_disclaim() -> bool {
    symbol(c"responsibility_spawnattrs_setdisclaim").is_some()
}

/// Looks up a function of libSystem that Apple does not declare publicly.
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

/// A child started responsible for itself.
#[cfg(target_os = "macos")]
pub struct Disclaimed {
    pub pid: libc::pid_t,
    /// Its standard input and output, when started with pipes.
    pub stdin: Option<std::fs::File>,
    pub stdout: Option<std::fs::File>,
}

/// Close descriptors this module opened (negative ones are not open).
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn close_all(fds: &[libc::c_int]) {
    for &fd in fds.iter().filter(|fd| **fd >= 0) {
        // SAFETY: closes a descriptor this module created and still owns.
        unsafe {
            libc::close(fd);
        }
    }
}

/// A pipe whose two ends close on exec: a child keeps only what its file
/// actions give it.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn pipe() -> Result<(libc::c_int, libc::c_int), String> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe writes two descriptors into the array; fcntl only sets
    // flags on descriptors this function owns.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(format!("pipe: {}", std::io::Error::last_os_error()));
        }
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    Ok((fds[0], fds[1]))
}

/// Starts this program again with `args`, responsible for itself, with
/// `extra_env` added. With `pipes`, its standard input and output are new
/// pipes returned to the caller; without, they and standard error are
/// /dev/null. Standard error is shared otherwise.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn spawn_disclaimed(
    args: &[std::ffi::OsString],
    extra_env: &[(&str, &str)],
    pipes: bool,
) -> Result<Disclaimed, String> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    type SetDisclaim = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, libc::c_int) -> libc::c_int;
    let set =
        symbol(c"responsibility_spawnattrs_setdisclaim").ok_or("macOS has no responsibility_spawnattrs_setdisclaim")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let cstr = |b: &[u8]| CString::new(b).map_err(|_| "NUL byte in an argument or the environment".to_string());
    let path = cstr(exe.as_os_str().as_bytes())?;
    let argv0 = std::env::args_os().next().unwrap_or_else(|| exe.clone().into_os_string());
    let args = std::iter::once(&argv0).chain(args).map(|a| cstr(a.as_bytes())).collect::<Result<Vec<_>, _>>()?;
    let overridden = |k: &std::ffi::OsStr| extra_env.iter().any(|(name, _)| k.as_bytes() == name.as_bytes());
    let mut env = std::env::vars_os()
        .filter(|(k, _)| !overridden(k))
        .map(|(k, v)| cstr(&[k.as_bytes(), b"=", v.as_bytes()].concat()))
        .collect::<Result<Vec<_>, _>>()?;
    for (k, v) in extra_env {
        env.push(cstr(format!("{k}={v}").as_bytes())?);
    }
    let argv: Vec<*mut libc::c_char> =
        args.iter().map(|a| a.as_ptr().cast_mut()).chain(std::iter::once(std::ptr::null_mut())).collect();
    let envp: Vec<*mut libc::c_char> =
        env.iter().map(|e| e.as_ptr().cast_mut()).chain(std::iter::once(std::ptr::null_mut())).collect();
    let (child_in, parent_in, parent_out, child_out) = if pipes {
        let (r_in, w_in) = pipe()?;
        let (r_out, w_out) = pipe().inspect_err(|_| close_all(&[r_in, w_in]))?;
        (r_in, w_in, r_out, w_out)
    } else {
        (-1, -1, -1, -1)
    };
    let fds = [child_in, parent_in, parent_out, child_out];

    // SAFETY: every pointer refers to a NUL-terminated string or array that
    // outlives the call; attributes and file actions are initialised before
    // use and destroyed after; the descriptors wrapped in File are ours
    // alone (the child's copies are dup2'ed onto 0 and 1, the originals
    // close on exec).
    unsafe {
        let set: SetDisclaim = std::mem::transmute::<*mut libc::c_void, SetDisclaim>(set);
        let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
        let mut actions: libc::posix_spawn_file_actions_t = std::ptr::null_mut();
        if libc::posix_spawnattr_init(&mut attr) != 0 {
            close_all(&fds);
            return Err("posix_spawn setup failed".into());
        }
        if libc::posix_spawn_file_actions_init(&mut actions) != 0 {
            libc::posix_spawnattr_destroy(&mut attr);
            close_all(&fds);
            return Err("posix_spawn setup failed".into());
        }
        if pipes {
            libc::posix_spawn_file_actions_adddup2(&mut actions, child_in, 0);
            libc::posix_spawn_file_actions_adddup2(&mut actions, child_out, 1);
        } else {
            let null = c"/dev/null";
            libc::posix_spawn_file_actions_addopen(&mut actions, 0, null.as_ptr(), libc::O_RDONLY, 0);
            libc::posix_spawn_file_actions_addopen(&mut actions, 1, null.as_ptr(), libc::O_WRONLY, 0);
            libc::posix_spawn_file_actions_addopen(&mut actions, 2, null.as_ptr(), libc::O_WRONLY, 0);
        }
        let mut pid: libc::pid_t = 0;
        let rc = match set(&mut attr, 1) {
            0 => libc::posix_spawn(&mut pid, path.as_ptr(), &actions, &attr, argv.as_ptr(), envp.as_ptr()),
            e => e,
        };
        libc::posix_spawn_file_actions_destroy(&mut actions);
        libc::posix_spawnattr_destroy(&mut attr);
        if pipes {
            libc::close(child_in);
            libc::close(child_out);
        }
        if rc != 0 {
            if pipes {
                libc::close(parent_in);
                libc::close(parent_out);
            }
            return Err(format!("posix_spawn: {}", std::io::Error::from_raw_os_error(rc)));
        }
        Ok(Disclaimed {
            pid,
            stdin: pipes.then(|| std::fs::File::from_raw_fd(parent_in)),
            stdout: pipes.then(|| std::fs::File::from_raw_fd(parent_out)),
        })
    }
}

/// Waits for a child and returns the code to exit with.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn wait(pid: libc::pid_t) -> i32 {
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
