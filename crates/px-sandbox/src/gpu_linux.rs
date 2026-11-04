//! The GPU process profile on Linux (ADR 033 (e), ADR 007's ladder, ADR 010).
//!
//! # Two stages, and why
//!
//! A GPU process must `execve` and dynamically link before it can run a line of
//! its own code, and both of those `open` files. A filter that denies `open`
//! cannot therefore be installed between `fork` and `exec`: the process would
//! die in the loader, and a filter that survived `exec` while allowing `open`
//! would allow it forever.
//!
//! So the policy has two stages, and the second only tightens:
//!
//! 1. **At creation** (`pre_exec`, the same filter every sandboxed child gets,
//!    see `linux::spawn`): `no_new_privs` and the escape-primitive deny list.
//!    The process has never run an unconfined instruction.
//! 2. **After startup**, by the process itself, as the first thing `main` does
//!    and before it reads a byte from the channel ([`enter`]): Landlock where
//!    the kernel has it, then a second seccomp filter denying the calls that
//!    reach the filesystem or the network. Seccomp filters stack and the kernel
//!    takes the strictest verdict, so stage two cannot loosen stage one, and
//!    `no_new_privs` means it cannot be undone.
//!
//! # Who vouches for stage two
//!
//! Not the child. A child describing its own confinement would be authority
//! taken from a message (invariant 9), and it is the one party that gains from
//! lying. The broker asks the kernel instead: [`verify_pid`] reads
//! `/proc/<pid>/status` and requires `NoNewPrivs: 1`, `Seccomp: 2` and **at
//! least two** installed filters. The broker does this after the child's
//! `Ready` message and before it sends any display list, so there is no
//! page-derived byte in the process until the kernel has confirmed both stages.
//! A kernel that does not report `Seccomp_filters` (before 5.9) cannot be
//! verified, and an unverifiable process is not used: fail closed.
//!
//! Landlock has no such window onto it: nothing in `/proc` says a process is
//! landlocked. It is therefore recorded as the process's report, never in the
//! verified list, and its effect is demonstrated by behaviour in the tests.
//!
//! # What stage two denies, and what it deliberately leaves
//!
//! It is a deny list, the weaker shape (see `linux.rs`: an allow list is Phase
//! 17's), so the honest statement is the list below and that anything not on it
//! is still allowed. In particular `stat` and `fstat` stay allowed (the
//! allocator and `std` use the latter; the former only leaks existence, and
//! Landlock closes it where present), as do `clone`, `futex`, `mmap` and the
//! rest of what an allocator, a thread runtime and a rasterizer need.
//!
//! Denied in stage two, on top of stage one:
//!
//! - **Opening anything by name or handle:** `open`, `openat`, `openat2`,
//!   `creat`, `name_to_handle_at`, `open_by_handle_at`.
//! - **Walking and changing the tree:** `getdents`, `getdents64`, `chdir`,
//!   `mkdir*`, `rmdir`, `unlink*`, `rename*`, `link*`, `symlink*`, `readlink*`,
//!   `access`/`faccessat*`, `chmod`, `fchmodat`, `chown`, `lchown`,
//!   `fchownat`, `mknod*`, `truncate`.
//! - **Sockets of every kind:** `socket`, `socketpair`, `connect`, `bind`,
//!   `listen`, `accept`, `accept4`.
//! - **Running anything else:** `execve`, `execveat`.
//! - **Doors that skip the ones above:** `io_uring_setup`, `io_uring_enter`,
//!   `io_uring_register` (io_uring performs opens and sockets inside the
//!   kernel, past this filter), the new mount API (`open_tree`, `move_mount`,
//!   `fsopen`, `fsconfig`, `fsmount`, `fspick`), and `pidfd_open` /
//!   `pidfd_getfd` (the latter steals descriptors from other processes).
//!
//! # Why the profile refuses a threaded caller
//!
//! `prctl(PR_SET_SECCOMP)` and `landlock_restrict_self` apply to the calling
//! thread. A process that had already spawned a thread would be confined on one
//! thread and free on the other, and nothing would say so. [`enter`] reads the
//! thread count first and refuses unless it is one.

use std::os::fd::{FromRawFd, OwnedFd};
use std::path::PathBuf;

use crate::linux;
use crate::{PolicyError, Restriction, Rung};

/// `landlock_create_ruleset` flag: return the highest ABI version.
const LANDLOCK_CREATE_RULESET_VERSION: libc::c_ulong = 1;
/// `landlock_add_rule` rule type: a path-beneath rule.
const LANDLOCK_RULE_PATH_BENEATH: libc::c_ulong = 1;

// Filesystem access rights, `linux/landlock.h`. ABI 1 defines bits 0..=12.
const FS_EXECUTE: u64 = 1 << 0;
const FS_READ_FILE: u64 = 1 << 2;
const FS_READ_DIR: u64 = 1 << 3;
const FS_ABI1_ALL: u64 = (1 << 13) - 1;
/// ABI 2.
const FS_REFER: u64 = 1 << 13;
/// ABI 3.
const FS_TRUNCATE: u64 = 1 << 14;
/// ABI 5.
const FS_IOCTL_DEV: u64 = 1 << 15;
/// ABI 4: TCP bind and connect.
const NET_ALL: u64 = 0b11;
/// ABI 6: abstract UNIX sockets and signals outside the domain.
const SCOPE_ALL: u64 = 0b11;

/// `struct landlock_ruleset_attr`, the ABI 6 layout. Older kernels take a
/// shorter prefix, selected by the size passed.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct RulesetAttr {
    pub(crate) handled_access_fs: u64,
    pub(crate) handled_access_net: u64,
    pub(crate) scoped: u64,
}

/// `struct landlock_path_beneath_attr`. Packed, as the kernel declares it.
#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// What a ruleset for `abi` must handle, and the size of the prefix of
/// [`RulesetAttr`] the kernel of that ABI understands.
///
/// Handling a right means *denying* it unless a rule grants it, so this asks
/// for every right the kernel knows: the profile's whole point is that nothing
/// is allowed that was not granted.
pub(crate) fn ruleset_for(abi: u32) -> (RulesetAttr, usize) {
    let mut fs = FS_ABI1_ALL;
    if abi >= 2 {
        fs |= FS_REFER;
    }
    if abi >= 3 {
        fs |= FS_TRUNCATE;
    }
    if abi >= 5 {
        fs |= FS_IOCTL_DEV;
    }
    let net = if abi >= 4 { NET_ALL } else { 0 };
    let scoped = if abi >= 6 { SCOPE_ALL } else { 0 };
    let size = if abi >= 6 {
        24
    } else if abi >= 4 {
        16
    } else {
        8
    };
    (
        RulesetAttr {
            handled_access_fs: fs,
            handled_access_net: net,
            scoped,
        },
        size,
    )
}

/// Classify what `landlock_create_ruleset(NULL, 0, VERSION)` returned.
///
/// Pure, so the ladder step is tested on machines of every kind. A positive
/// result is the ABI. `ENOSYS` (no such syscall) and `EOPNOTSUPP` (compiled in
/// but disabled at boot) are "the kernel does not offer it". Anything else, or
/// a non-positive result with no error, is inconclusive, which counts as
/// unavailable (ADR 007): the mistake to avoid is believing Landlock is on.
pub(crate) fn classify_landlock_version(result: i64, errno: i32) -> Result<u32, Restriction> {
    const ENOSYS: i32 = 38;
    const EOPNOTSUPP: i32 = 95;
    if result >= 1 {
        return Ok(u32::try_from(result).unwrap_or(u32::MAX));
    }
    match errno {
        ENOSYS | EOPNOTSUPP if result < 0 => Err(Restriction::NotSupported),
        _ => Err(Restriction::Inconclusive),
    }
}

/// The Landlock ABI version this kernel enforces, or why it does not.
///
/// This is the documented way to ask (`Documentation/userspace-api/landlock`):
/// the version query takes no pointer and changes nothing. It replaces reading
/// the active-LSM list under securityfs, which is not mounted in every
/// environment (a container, or a sandbox of the kind this runs in) and so made
/// Landlock look absent where it was present.
pub(crate) fn landlock_abi() -> Result<u32, Restriction> {
    // SAFETY: with a null attribute pointer, size 0 and the VERSION flag,
    // landlock_create_ruleset(2) dereferences nothing and creates nothing: it
    // returns the ABI version. The variadic arguments are a pointer-sized null,
    // a size and a flags word, each passed at the width of c_ulong/pointer as
    // `syscall(2)` expects for this call.
    let result: i64 = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0 as libc::c_ulong,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    classify_landlock_version(result, errno)
}

/// Stage-two additions to the deny list, as syscall numbers.
///
/// Built from `libc`'s own constants rather than numerals, so the name is what
/// is reviewed and the number is the platform's. Refuses (rather than drops)
/// a constant that does not fit: a denial silently missing from the list would
/// read as protection.
fn stage_two_denied() -> Result<Vec<u32>, PolicyError> {
    let mut numbers: Vec<libc::c_long> = vec![
        libc::SYS_openat,
        libc::SYS_openat2,
        libc::SYS_name_to_handle_at,
        libc::SYS_open_by_handle_at,
        libc::SYS_getdents64,
        libc::SYS_chdir,
        libc::SYS_mkdirat,
        libc::SYS_unlinkat,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_linkat,
        libc::SYS_symlinkat,
        libc::SYS_readlinkat,
        libc::SYS_faccessat,
        libc::SYS_faccessat2,
        libc::SYS_fchmodat,
        libc::SYS_fchownat,
        libc::SYS_mknodat,
        libc::SYS_truncate,
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_open_tree,
        libc::SYS_move_mount,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_fspick,
        libc::SYS_pidfd_open,
        libc::SYS_pidfd_getfd,
    ];
    // The pre-`*at` spellings exist only where the architecture kept them.
    #[cfg(target_arch = "x86_64")]
    numbers.extend([
        libc::SYS_open,
        libc::SYS_creat,
        libc::SYS_getdents,
        libc::SYS_mkdir,
        libc::SYS_rmdir,
        libc::SYS_unlink,
        libc::SYS_rename,
        libc::SYS_link,
        libc::SYS_symlink,
        libc::SYS_readlink,
        libc::SYS_access,
        libc::SYS_chmod,
        libc::SYS_chown,
        libc::SYS_lchown,
        libc::SYS_mknod,
    ]);

    let mut denied: Vec<u32> = linux::DENIED_SYSCALLS.to_vec();
    for number in numbers {
        let number = u32::try_from(number).map_err(|_| PolicyError::Failed {
            rung: Rung::Seccomp,
            detail: "a syscall number does not fit the filter",
        })?;
        if !denied.contains(&number) {
            denied.push(number);
        }
    }
    if denied.len() >= 250 {
        return Err(PolicyError::Failed {
            rung: Rung::Seccomp,
            detail: "the stage-two deny list is too long for a u8 BPF jump",
        });
    }
    Ok(denied)
}

/// What [`enter`] managed.
pub(crate) struct Entered {
    /// Rungs applied by this process to itself.
    pub(crate) applied: Vec<Rung>,
    /// The Landlock ABI enforced, or 0.
    pub(crate) landlock_abi: u32,
}

/// Apply stage two to the calling process. See the module documentation.
pub(crate) fn enter(read_only: &[PathBuf]) -> Result<Entered, PolicyError> {
    let failed = |rung, detail| PolicyError::Failed { rung, detail };

    require_single_threaded()?;

    // Everything that allocates or reads a file happens before the first
    // restriction lands, because after it neither is possible.
    let denied = stage_two_denied()?;
    let program = linux::build_filter_for(&denied);
    let grants = open_grants(read_only)?;

    linux::set_no_new_privs()?;

    // Landlock first, seccomp second: both are one-way, and the seccomp filter
    // would otherwise deny the very syscalls Landlock's setup needs.
    let abi = landlock_abi().unwrap_or(0);
    if abi >= 1 {
        apply_landlock(abi, &grants)?;
    }

    linux::install_filter(&program)?;

    // Behavioural self-check. Not authority, which is the broker's, and
    // verified from outside, but a process that finds the door still open
    // should not carry on and wait to be told.
    match std::fs::File::open("/proc/self/status") {
        Ok(_) => Err(failed(
            Rung::Seccomp,
            "after the profile, open still succeeds",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(Entered {
            applied: {
                let mut applied = vec![Rung::NoNewPrivs, Rung::Seccomp];
                if abi >= 1 {
                    applied.push(Rung::Landlock);
                }
                applied
            },
            landlock_abi: abi,
        }),
        Err(_) => Err(failed(
            Rung::Seccomp,
            "after the profile, open failed for a reason other than denial",
        )),
    }
}

fn require_single_threaded() -> Result<(), PolicyError> {
    let status = std::fs::read_to_string("/proc/self/status").map_err(|_| PolicyError::Failed {
        rung: Rung::Seccomp,
        detail: "the thread count could not be read",
    })?;
    let threads = status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .map(str::trim);
    if threads == Some("1") {
        Ok(())
    } else {
        Err(PolicyError::Failed {
            rung: Rung::Seccomp,
            detail: "the profile applies per thread and this process is not single-threaded",
        })
    }
}

/// A path the process is granted read access to, opened before anything is
/// restricted. `O_PATH` descriptors confer no access by themselves; they are
/// what Landlock rules are made from.
struct Grant {
    file: std::fs::File,
    is_dir: bool,
}

fn open_grants(read_only: &[PathBuf]) -> Result<Vec<Grant>, PolicyError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut grants = Vec::with_capacity(read_only.len());
    for path in read_only {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| PolicyError::Failed {
                rung: Rung::Landlock,
                detail: "a granted path could not be opened",
            })?;
        let is_dir = std::fs::metadata(path)
            .map(|meta| meta.is_dir())
            .map_err(|_| PolicyError::Failed {
                rung: Rung::Landlock,
                detail: "a granted path could not be inspected",
            })?;
        grants.push(Grant { file, is_dir });
    }
    Ok(grants)
}

fn apply_landlock(abi: u32, grants: &[Grant]) -> Result<(), PolicyError> {
    use std::os::fd::AsRawFd;
    let failed = |detail| PolicyError::Failed {
        rung: Rung::Landlock,
        detail,
    };

    let (attr, size) = ruleset_for(abi);
    // SAFETY: landlock_create_ruleset(2) reads `size` bytes from the attribute
    // pointer. `attr` is a live #[repr(C)] landlock_ruleset_attr in the ABI 6
    // layout, and `size` is at most its length (8, 16 or 24 of its 24 bytes),
    // so the read stays inside it. The kernel copies the attribute and keeps no
    // reference.
    let created = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::addr_of!(attr),
            size as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    let raw =
        i32::try_from(created).map_err(|_| failed("the ruleset descriptor is out of range"))?;
    if raw < 0 {
        return Err(failed("landlock_create_ruleset was refused"));
    }
    // SAFETY: `raw` is a descriptor the kernel has just returned from a
    // successful landlock_create_ruleset(2); nothing else owns it, so taking
    // ownership here closes it exactly once.
    let ruleset = unsafe { OwnedFd::from_raw_fd(raw) };

    for grant in grants {
        let mut allowed = FS_READ_FILE;
        if grant.is_dir {
            // Directory-only rights on a file are EINVAL, so they are
            // requested only for a directory.
            allowed |= FS_READ_DIR;
        }
        // Execute is never granted: a read-only grant is data, not code.
        allowed &= attr.handled_access_fs & !FS_EXECUTE;
        let rule = PathBeneathAttr {
            allowed_access: allowed,
            parent_fd: grant.file.as_raw_fd(),
        };
        // SAFETY: landlock_add_rule(2) with LANDLOCK_RULE_PATH_BENEATH reads a
        // struct landlock_path_beneath_attr from the pointer. `rule` is a live
        // packed #[repr(C)] value of exactly that layout, so the read is in
        // bounds; the pointer is taken with addr_of! so no reference to the
        // packed field is formed. `ruleset` is the descriptor from above and
        // `parent_fd` is an O_PATH descriptor owned by `grant`, both open for
        // the call.
        let added = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                LANDLOCK_RULE_PATH_BENEATH,
                std::ptr::addr_of!(rule),
                0 as libc::c_ulong,
            )
        };
        if added != 0 {
            return Err(failed("landlock_add_rule was refused"));
        }
    }

    // SAFETY: landlock_restrict_self(2) takes the ruleset descriptor and a
    // flags word of zero, reads no memory, and confines the calling thread.
    // no_new_privs, which it requires, was set by the caller.
    let restricted = unsafe {
        libc::syscall(
            libc::SYS_landlock_restrict_self,
            ruleset.as_raw_fd(),
            0 as libc::c_ulong,
        )
    };
    if restricted != 0 {
        return Err(failed("landlock_restrict_self was refused"));
    }
    Ok(())
}

/// What the kernel says about a process's confinement, judged.
///
/// Pure over the text of `/proc/<pid>/status`, so it is tested with every
/// shape of that file (old kernels, partial confinement, hostile spellings).
pub(crate) fn judge_status(status: &str) -> Result<Vec<Rung>, &'static str> {
    let field = |name: &str| -> Option<&str> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name).and_then(|r| r.strip_prefix(':')))
            .map(str::trim)
    };
    if field("NoNewPrivs") != Some("1") {
        return Err("the kernel does not report no_new_privs set");
    }
    if field("Seccomp") != Some("2") {
        return Err("the kernel does not report seccomp in filter mode");
    }
    match field("Seccomp_filters").and_then(|value| value.parse::<u32>().ok()) {
        Some(count) if count >= 2 => Ok(vec![Rung::NoNewPrivs, Rung::Seccomp]),
        Some(_) => Err("fewer than two seccomp filters are installed: stage two is not in place"),
        None => Err("the kernel does not report the filter count, so stage two cannot be verified"),
    }
}

/// Read the kernel's view of `pid` and judge it. See the module documentation.
pub(crate) fn verify_pid(pid: u32) -> Result<Vec<Rung>, PolicyError> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).map_err(|_| {
        PolicyError::Failed {
            rung: Rung::Seccomp,
            detail: "the process's confinement could not be read from the kernel",
        }
    })?;
    judge_status(&text).map_err(|detail| PolicyError::Failed {
        rung: Rung::Seccomp,
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_landlock_version_probe_is_classified_fail_closed() {
        assert_eq!(classify_landlock_version(7, 0), Ok(7));
        assert_eq!(classify_landlock_version(1, 0), Ok(1));
        assert_eq!(
            classify_landlock_version(-1, 38),
            Err(Restriction::NotSupported)
        );
        assert_eq!(
            classify_landlock_version(-1, 95),
            Err(Restriction::NotSupported)
        );
        // Anything else is not "available".
        assert_eq!(
            classify_landlock_version(-1, 1),
            Err(Restriction::Inconclusive)
        );
        assert_eq!(
            classify_landlock_version(0, 0),
            Err(Restriction::Inconclusive)
        );
        assert_eq!(
            classify_landlock_version(0, 38),
            Err(Restriction::Inconclusive)
        );
    }

    #[test]
    fn this_machines_landlock_answer_agrees_with_the_classifier() {
        // Whatever the kernel says, it must be one of the two classified
        // outcomes, and detection must agree with it.
        let direct = landlock_abi();
        let detected = crate::detect().has(Rung::Landlock);
        assert_eq!(direct.is_ok(), detected);
    }

    #[test]
    fn a_ruleset_handles_every_right_its_abi_knows() {
        let (v1, size1) = ruleset_for(1);
        assert_eq!(v1.handled_access_fs, (1 << 13) - 1);
        assert_eq!((v1.handled_access_net, v1.scoped, size1), (0, 0, 8));
        let (v3, _) = ruleset_for(3);
        assert_eq!(v3.handled_access_fs, (1 << 15) - 1);
        let (v4, size4) = ruleset_for(4);
        assert_eq!((v4.handled_access_net, size4), (3, 16));
        let (v6, size6) = ruleset_for(6);
        assert_eq!((v6.scoped, size6), (3, 24));
        assert_eq!(size6, std::mem::size_of::<RulesetAttr>());
        // Execute is among what is handled, so it is denied unless granted.
        assert_ne!(v6.handled_access_fs & FS_EXECUTE, 0);
    }

    const GOOD: &str = "Name:\tx\nNoNewPrivs:\t1\nSeccomp:\t2\nSeccomp_filters:\t2\n";

    #[test]
    fn a_status_with_both_stages_is_verified() {
        assert_eq!(
            judge_status(GOOD),
            Ok(vec![Rung::NoNewPrivs, Rung::Seccomp])
        );
    }

    #[test]
    fn a_status_missing_either_stage_is_refused() {
        for (bad, why) in [
            (
                "NoNewPrivs:\t0\nSeccomp:\t2\nSeccomp_filters:\t2\n",
                "no_new_privs",
            ),
            (
                "NoNewPrivs:\t1\nSeccomp:\t0\nSeccomp_filters:\t0\n",
                "filter mode",
            ),
            (
                "NoNewPrivs:\t1\nSeccomp:\t1\nSeccomp_filters:\t2\n",
                "filter mode",
            ),
            (
                "NoNewPrivs:\t1\nSeccomp:\t2\nSeccomp_filters:\t1\n",
                "stage two",
            ),
            (
                "NoNewPrivs:\t1\nSeccomp:\t2\nSeccomp_filters:\tmany\n",
                "cannot be verified",
            ),
            // An old kernel: no filter count at all.
            ("NoNewPrivs:\t1\nSeccomp:\t2\n", "cannot be verified"),
            ("", "no_new_privs"),
        ] {
            let error = judge_status(bad).expect_err(bad);
            assert!(error.contains(why), "{bad:?} gave {error}");
        }
    }

    /// A field that merely contains the name must not satisfy the check.
    #[test]
    fn a_lookalike_field_does_not_satisfy_the_check() {
        let forged = "XNoNewPrivs:\t1\nSeccomp:\t2\nSeccomp_filters:\t2\n";
        assert!(judge_status(forged).is_err());
    }

    #[test]
    fn the_stage_two_list_includes_stage_one_and_fits_a_jump() {
        let denied = stage_two_denied().expect("list");
        for stage_one in linux::DENIED_SYSCALLS {
            assert!(denied.contains(stage_one));
        }
        assert!(denied.len() < 250);
        let mut sorted = denied.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), denied.len(), "no number listed twice");
        for must in [libc::SYS_openat, libc::SYS_socket, libc::SYS_connect] {
            assert!(denied.contains(&(must as u32)));
        }
    }

    #[test]
    fn the_stage_two_filter_jumps_every_comparison_to_the_deny() {
        let denied = stage_two_denied().expect("list");
        let program = linux::build_filter_for(&denied);
        let deny_index = program.len() - 1;
        for offset in 0..denied.len() {
            let index = 4 + offset;
            let instruction = program[index];
            assert_eq!(index + 1 + usize::from(instruction.jt), deny_index);
        }
    }
}
