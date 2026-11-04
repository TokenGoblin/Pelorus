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
}
