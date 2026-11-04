//! Testing only: attempt the operations a GPU process must not be able to do.
//!
//! §14.4: this module exists only under `cfg(feature = "testing")` and the
//! release-artifact scan looks for [`MARKER`] in the shipped binaries. It lives
//! here, not in the GPU crate, because making a socket is unsafe FFI (the
//! standard library's socket types are also off limits outside the network
//! crates, by the symbol audit) and this is the one crate that may.
//!
//! The probe reports what the *kernel answered*. That is evidence about the
//! process's behaviour and never about its authority (invariant 9): the
//! broker's verdict on confinement comes from the operating system's view of
//! the process, and this is the demonstration that the verdict means something.
//!
//! `connect` and `bind` are probed on a descriptor that does not exist (`-1`).
//! A kernel with no policy answers `EBADF`, because it checks the descriptor
//! first; a seccomp filter answers `EPERM` before it ever looks. The two
//! answers differ exactly when the policy is present, and no network operation
//! has to be allowed to find out.

/// A string that exists in a binary if and only if this module was compiled in.
pub const MARKER: &str = "px-sandbox-testing-probe-of-forbidden-operations";

/// A second marker, for the content-profile escape suite (Phase 17). It exists
/// in a binary if and only if [`escape_attempts`] was compiled in and called;
/// `cargo xtask sandbox` scans every shipped binary for it.
pub const ESCAPE_MARKER: &str = "px-sandbox-testing-escape-suite-of-the-content-profile";

/// What was attempted.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Open a world-readable system file.
    OpenSystemFile,
    /// Open the working directory.
    OpenWorkingDirectory,
    /// Create an Internet-domain socket.
    Socket,
    /// `connect` on a descriptor that does not exist.
    Connect,
    /// `bind` on a descriptor that does not exist.
    Bind,
    /// `execve` of a path that does not exist.
    Exec,
}

/// One result.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// What.
    pub op: Op,
    /// Whether the operation worked.
    pub succeeded: bool,
    /// `errno` when it did not, else 0.
    pub errno: i32,
}

impl Attempt {
    /// Whether the failure was a *denial* (`EPERM` from seccomp, `EACCES` from
    /// Landlock), as opposed to some other failure that would have happened
    /// anyway.
    pub fn denied(&self) -> bool {
        !self.succeeded && (self.errno == 1 || self.errno == 13)
    }
}

/// Attempt every forbidden operation and report each result.
#[cfg(target_os = "linux")]
pub fn forbidden_operations() -> Vec<Attempt> {
    // Keep the marker alive in the binary; a constant nothing reads is dropped.
    std::hint::black_box(MARKER);

    let from_io = |op, result: std::io::Result<std::fs::File>| match result {
        Ok(_) => Attempt {
            op,
            succeeded: true,
            errno: 0,
        },
        Err(error) => Attempt {
            op,
            succeeded: false,
            errno: error.raw_os_error().unwrap_or(-1),
        },
    };
    let from_raw = |op, result: i32| Attempt {
        op,
        succeeded: result >= 0,
        errno: if result >= 0 {
            0
        } else {
            std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
        },
    };

    let mut attempts = Vec::new();
    attempts.push(from_io(
        Op::OpenSystemFile,
        std::fs::File::open("/etc/passwd"),
    ));
    attempts.push(from_io(Op::OpenWorkingDirectory, std::fs::File::open(".")));

    // SAFETY: socket(2) takes three integers and touches no memory. A
    // descriptor it returns is closed immediately below.
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    attempts.push(from_raw(Op::Socket, socket));
    if socket >= 0 {
        // SAFETY: `socket` is a descriptor just returned by socket(2) and
        // owned by nobody else; closing it once is correct.
        unsafe {
            libc::close(socket);
        }
    }

    let address = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 9u16.to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_be_bytes([127, 0, 0, 1]).to_be(),
        },
        sin_zero: [0; 8],
    };
    let length = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: connect(2) reads `length` bytes from the address pointer. It
    // points at `address`, a live sockaddr_in of exactly that size. The
    // descriptor is -1, which no process has open, so even a kernel with no
    // policy fails the call with EBADF and connects to nothing.
    let connect = unsafe {
        libc::connect(
            -1,
            std::ptr::addr_of!(address).cast::<libc::sockaddr>(),
            length,
        )
    };
    attempts.push(from_raw(Op::Connect, connect));
    // SAFETY: as for connect: a live sockaddr_in of `length` bytes, and a
    // descriptor that is not open, so nothing is bound.
    let bind = unsafe {
        libc::bind(
            -1,
            std::ptr::addr_of!(address).cast::<libc::sockaddr>(),
            length,
        )
    };
    attempts.push(from_raw(Op::Bind, bind));

    let path = c"/nonexistent-probe-target";
    let argv: [*const libc::c_char; 1] = [std::ptr::null()];
    // SAFETY: execve(2) reads a NUL-terminated path and two NULL-terminated
    // pointer arrays. `path` is a C string literal, `argv` is a one-element
    // array holding the terminating NULL, and a null `envp` is permitted on
    // Linux as an empty environment. The path does not exist, so a kernel with
    // no policy fails with ENOENT and the process image is never replaced.
    let exec = unsafe { libc::execve(path.as_ptr(), argv.as_ptr(), std::ptr::null()) };
    attempts.push(from_raw(Op::Exec, exec));
    attempts
}

/// The escape attempts of the content-profile suite, by name and in the order
/// [`escape_attempts`] reports them (Phase 17; ADR 043). The names are the wire
/// vocabulary: a test names what it expects, and a missing or extra name is a
/// failure rather than something to skip.
pub const ESCAPES: &[&str] = &[
    "open_file",
    "open_dev_null",
    "enumerate_dev",
    "open_dev_dri",
    "create_file",
    "mkdir",
    "unlink",
    "rename",
    "socket_inet",
    "socket_unix",
    "connect_inet",
    "connect_unix_display",
    "bind_inet",
    "exec",
    "spawn_process",
    "read_peer_proc_mem",
    "read_peer_vm",
    "ptrace_peer",
    "mount",
    "unshare",
    "setns",
    "chroot",
    "io_uring",
    "perf_event",
    "bpf",
    "pidfd_open_peer",
    "signal_peer",
    "stat_file",
];

/// One escape attempt and what the kernel answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapeAttempt {
    /// One of [`ESCAPES`].
    pub name: &'static str,
    /// Whether the operation worked.
    pub succeeded: bool,
    /// `errno` when it failed, else 0.
    pub errno: i32,
}

/// Everything [`escape_attempts`] learned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapeReport {
    /// The Landlock ABI the kernel enforces (0 if none): which of the attempts
    /// Landlock can independently refuse depends on it.
    pub landlock_abi: u32,
    /// One entry per name in [`ESCAPES`], in that order.
    pub attempts: Vec<EscapeAttempt>,
}

/// Attempt every escape and report each result.
///
/// Each attempt is built so that an unconfined process fails it for an
/// *ordinary* reason (`ENOENT`, `EBADF`, `EFAULT`, a bad argument) or succeeds
/// harmlessly, never by doing damage: nothing here can mount, signal, trace or
/// remove anything real. A confined process answers `EPERM` (seccomp) or
/// `EACCES` (Landlock) instead, and the difference between the two answers is
/// the evidence. `an_unconfined_process_is_denied_nothing` is the control.
#[cfg(target_os = "linux")]
pub fn escape_attempts() -> EscapeReport {
    // Keep both markers alive in the binary; a constant nothing reads is dropped.
    std::hint::black_box(MARKER);
    std::hint::black_box(ESCAPE_MARKER);

    fn io(name: &'static str, result: std::io::Result<()>) -> EscapeAttempt {
        match result {
            Ok(()) => EscapeAttempt {
                name,
                succeeded: true,
                errno: 0,
            },
            Err(error) => EscapeAttempt {
                name,
                succeeded: false,
                errno: error.raw_os_error().unwrap_or(-1),
            },
        }
    }
    fn raw(name: &'static str, result: i64) -> EscapeAttempt {
        // errno is read before anything else can overwrite it.
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
        EscapeAttempt {
            name,
            succeeded: result >= 0,
            errno: if result >= 0 { 0 } else { errno },
        }
    }
    let close = |fd: i64| {
        if fd >= 0 {
            // SAFETY: `fd` was just returned by a successful system call and
            // is owned by nobody else; closing it once is correct.
            unsafe {
                libc::close(fd as i32);
            }
        }
    };

    let mut attempts = Vec::with_capacity(ESCAPES.len());
    // SAFETY: getppid(2) takes no arguments and cannot fail.
    let parent = unsafe { libc::getppid() };
    let pid = std::process::id();

    // ---- the filesystem: read, enumerate, write -----------------------------
    attempts.push(io(
        "open_file",
        std::fs::File::open("/etc/passwd").map(drop),
    ));
    attempts.push(io(
        "open_dev_null",
        std::fs::File::open("/dev/null").map(drop),
    ));
    attempts.push(io("enumerate_dev", std::fs::read_dir("/dev").map(drop)));
    attempts.push(io(
        "open_dev_dri",
        std::fs::File::open("/dev/dri/renderD128").map(drop),
    ));
    let scratch = format!("/tmp/px-escape-probe-{pid}");
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch)
        .map(drop);
    if created.is_ok() {
        // Only reachable unconfined: leave nothing behind.
        let _ = std::fs::remove_file(&scratch);
    }
    attempts.push(io("create_file", created));
    let scratch_dir = format!("/tmp/px-escape-probe-dir-{pid}");
    let made = std::fs::create_dir(&scratch_dir);
    if made.is_ok() {
        let _ = std::fs::remove_dir(&scratch_dir);
    }
    attempts.push(io("mkdir", made));
    // Removing and renaming a path that does not exist: unconfined, ENOENT.
    attempts.push(io(
        "unlink",
        std::fs::remove_file("/tmp/px-escape-probe-missing"),
    ));
    attempts.push(io(
        "rename",
        std::fs::rename(
            "/tmp/px-escape-probe-missing",
            "/tmp/px-escape-probe-missing-too",
        ),
    ));

    // ---- the network and the display ----------------------------------------
    // SAFETY: socket(2) takes three integers and touches no memory.
    let inet = i64::from(unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) });
    attempts.push(raw("socket_inet", inet));
    close(inet);
    // SAFETY: as above.
    let unix = i64::from(unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) });
    attempts.push(raw("socket_unix", unix));
    close(unix);

    let address = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 9u16.to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_be_bytes([127, 0, 0, 1]).to_be(),
        },
        sin_zero: [0; 8],
    };
    let length = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: connect(2) reads `length` bytes from the pointer, which is a
    // live sockaddr_in of exactly that size. The descriptor is -1, which no
    // process has open, so an unconfined kernel answers EBADF and connects to
    // nothing.
    let connect = unsafe {
        libc::connect(
            -1,
            std::ptr::addr_of!(address).cast::<libc::sockaddr>(),
            length,
        )
    };
    attempts.push(raw("connect_inet", i64::from(connect)));

    // A display server is a UNIX socket: this is the X11 socket's path, and the
    // Wayland socket is the same call with another path. The socket() attempt
    // above is what stops a real connection; this one shows connect itself is
    // denied (clipboard access on Linux is a connection to one of these).
    let mut display: libc::sockaddr_un = {
        // SAFETY: sockaddr_un is plain data for which all-zero is valid.
        unsafe { std::mem::zeroed() }
    };
    display.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in display.sun_path.iter_mut().zip(b"/tmp/.X11-unix/X0") {
        *slot = *byte as libc::c_char;
    }
    let display_length = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    // SAFETY: as for connect above: a live sockaddr_un of `display_length`
    // bytes and a descriptor that is not open, so nothing is connected.
    let connect_unix = unsafe {
        libc::connect(
            -1,
            std::ptr::addr_of!(display).cast::<libc::sockaddr>(),
            display_length,
        )
    };
    attempts.push(raw("connect_unix_display", i64::from(connect_unix)));

    // SAFETY: as for connect: a live sockaddr_in and a descriptor not open.
    let bind = unsafe {
        libc::bind(
            -1,
            std::ptr::addr_of!(address).cast::<libc::sockaddr>(),
            length,
        )
    };
    attempts.push(raw("bind_inet", i64::from(bind)));

    // ---- running something else ----------------------------------------------
    let path = c"/nonexistent-probe-target";
    let argv: [*const libc::c_char; 1] = [std::ptr::null()];
    // SAFETY: execve(2) reads a NUL-terminated path and two NULL-terminated
    // pointer arrays; `path` is a C string literal, `argv` holds the
    // terminating NULL, and a null `envp` is permitted on Linux. The path does
    // not exist, so an unconfined kernel fails with ENOENT and the image is
    // never replaced.
    let exec = unsafe { libc::execve(path.as_ptr(), argv.as_ptr(), std::ptr::null()) };
    attempts.push(raw("exec", i64::from(exec)));
    // The whole fork-and-exec path, through the standard library. Standard
    // streams are inherited rather than redirected, so nothing is opened here
    // that the filter could refuse for another reason.
    let spawned = std::process::Command::new("/bin/true").spawn();
    attempts.push(match spawned {
        Ok(mut child) => {
            let _ = child.wait();
            io("spawn_process", Ok(()))
        }
        Err(error) => io("spawn_process", Err(error)),
    });

    // ---- another process's memory --------------------------------------------
    attempts.push(io(
        "read_peer_proc_mem",
        std::fs::File::open(format!("/proc/{parent}/mem")).map(drop),
    ));
    let mut local = [0u8; 8];
    let local_iov = libc::iovec {
        iov_base: local.as_mut_ptr().cast(),
        iov_len: local.len(),
    };
    let remote_iov = libc::iovec {
        iov_base: 0x1000 as *mut libc::c_void,
        iov_len: local.len(),
    };
    // SAFETY: process_vm_readv(2) writes at most `iov_len` bytes into the local
    // iovec, which points at `local`, a live 8-byte buffer, and reads the
    // remote iovec only as numbers: the remote address is in another process
    // and is never dereferenced here. An unconfined kernel answers EFAULT (the
    // address is not mapped there) or EPERM, never data.
    let vm = unsafe { libc::process_vm_readv(parent, &local_iov, 1, &remote_iov, 1, 0) };
    attempts.push(raw("read_peer_vm", vm as i64));
    // PEEKDATA on a process that is not our tracee: unconfined, ESRCH. (An
    // attach would stop the peer if it worked, so it is not used.)
    // SAFETY: ptrace(2) PTRACE_PEEKDATA with a null address and data reads no
    // memory of ours; the kernel validates the pid and the tracee relation.
    let ptrace = unsafe {
        libc::ptrace(
            libc::PTRACE_PEEKDATA,
            parent,
            std::ptr::null_mut::<libc::c_void>(),
            std::ptr::null_mut::<libc::c_void>(),
        )
    };
    attempts.push(raw("ptrace_peer", ptrace as i64));

    // ---- reshaping the sandbox and the kernel ---------------------------------
    // SAFETY: mount(2) reads three NUL-terminated strings, all C literals; the
    // data pointer is null. The target does not exist, so an unconfined kernel
    // fails with ENOENT before changing anything.
    let mount = unsafe {
        libc::mount(
            c"none".as_ptr(),
            c"/nonexistent-probe-target".as_ptr(),
            c"tmpfs".as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    attempts.push(raw("mount", i64::from(mount)));
    // SAFETY: unshare(2) with no flags is a documented no-op that changes
    // nothing; a confined kernel refuses it before looking at the flags.
    let unshare = unsafe { libc::unshare(0) };
    attempts.push(raw("unshare", i64::from(unshare)));
    // SAFETY: setns(2) on descriptor -1: unconfined, EBADF.
    let setns = unsafe { libc::setns(-1, 0) };
    attempts.push(raw("setns", i64::from(setns)));
    // SAFETY: chroot(2) reads a NUL-terminated C literal for a path that does
    // not exist: unconfined, ENOENT, and the root is unchanged.
    let chroot = unsafe { libc::chroot(c"/nonexistent-probe-target".as_ptr()) };
    attempts.push(raw("chroot", i64::from(chroot)));
    // SAFETY: io_uring_setup(2) with zero entries and a null parameter block
    // reads no memory: unconfined, EFAULT or EINVAL, and no ring is created.
    let ring = unsafe { libc::syscall(libc::SYS_io_uring_setup, 0, std::ptr::null::<u8>()) };
    attempts.push(raw("io_uring", ring));
    // SAFETY: perf_event_open(2) with a null attribute pointer: unconfined,
    // EFAULT, and no event is opened.
    let perf = unsafe {
        libc::syscall(
            libc::SYS_perf_event_open,
            std::ptr::null::<u8>(),
            0,
            -1,
            -1,
            0,
        )
    };
    attempts.push(raw("perf_event", perf));
    // SAFETY: bpf(2) with command 0 (BPF_MAP_CREATE) and a null attribute
    // pointer of size 0: unconfined, EINVAL or EFAULT, and nothing is created.
    let bpf = unsafe { libc::syscall(libc::SYS_bpf, 0, std::ptr::null::<u8>(), 0) };
    attempts.push(raw("bpf", bpf));
    // SAFETY: pidfd_open(2) takes a pid and flags and touches no memory.
    // Unconfined it succeeds; the descriptor is closed at once.
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, parent, 0) };
    attempts.push(raw("pidfd_open_peer", pidfd));
    close(pidfd);
    // Signal zero delivers nothing; it only asks whether the peer could be
    // signalled. Landlock's signal scope (ABI 6) refuses it for a process
    // outside the domain; seccomp as configured does not.
    // SAFETY: kill(2) with signal 0 performs only the permission check.
    let signal = unsafe { libc::kill(parent, 0) };
    attempts.push(raw("signal_peer", i64::from(signal)));
    // Metadata is not an open: stat stays allowed (it leaks existence, not
    // content), and this attempt records that so a change is noticed.
    attempts.push(io("stat_file", std::fs::metadata("/etc/passwd").map(drop)));

    EscapeReport {
        landlock_abi: crate::gpu_linux::landlock_abi().unwrap_or(0),
        attempts,
    }
}

/// There is no escape probe on this platform.
#[cfg(not(target_os = "linux"))]
pub fn escape_attempts() -> EscapeReport {
    EscapeReport {
        landlock_abi: 0,
        attempts: Vec::new(),
    }
}

/// There is no probe on this platform: an empty report, which no test mistakes
/// for a pass because each asserts the operations it expects to see.
#[cfg(not(target_os = "linux"))]
pub fn forbidden_operations() -> Vec<Attempt> {
    Vec::new()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// The control that makes the denial tests mean something: in this
    /// unconfined test process the probe sees the operations *work* (or fail
    /// for the ordinary reason), so a denial elsewhere is the policy and not
    /// the probe being unable to succeed.
    #[test]
    fn an_unconfined_process_is_not_denied_anything() {
        let attempts = forbidden_operations();
        assert_eq!(attempts.len(), 6);
        for attempt in &attempts {
            assert!(!attempt.denied(), "unconfined, yet denied: {attempt:?}");
        }
        let find = |op| attempts.iter().find(|a| a.op == op).copied();
        assert!(find(Op::OpenSystemFile).is_some_and(|a| a.succeeded));
        assert!(find(Op::OpenWorkingDirectory).is_some_and(|a| a.succeeded));
        // EBADF (9) and ENOENT (2): the answers of a kernel with no policy.
        assert_eq!(find(Op::Connect).map(|a| a.errno), Some(9));
        assert_eq!(find(Op::Bind).map(|a| a.errno), Some(9));
        assert_eq!(find(Op::Exec).map(|a| a.errno), Some(2));
    }

    /// The control for the content-profile suite: unconfined, the probe reports
    /// every name in order, and the operations that can simply work do work, so
    /// a refusal elsewhere is the policy and not the probe being unable to
    /// succeed.
    #[test]
    fn sandbox_escape_probe_unconfined_control() {
        let report = escape_attempts();
        let names: Vec<&str> = report.attempts.iter().map(|a| a.name).collect();
        assert_eq!(names, ESCAPES, "one result per name, in order");
        let find = |name| report.attempts.iter().find(|a| a.name == name).expect(name);
        for works in ["open_file", "open_dev_null", "stat_file", "spawn_process"] {
            assert!(find(works).succeeded, "unconfined, {works} should work");
        }
        // The ordinary answers of a kernel with no policy.
        assert_eq!(find("connect_inet").errno, 9);
        assert_eq!(find("exec").errno, 2);
        assert_eq!(find("chroot").errno, 2);
        assert_eq!(find("unlink").errno, 2);
    }
}
