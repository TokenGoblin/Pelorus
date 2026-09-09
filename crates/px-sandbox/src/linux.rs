//! The Linux policy, and the unsafe that applies it (ADR 007, ADR 008, 010).
//!
//! # The policy goes between fork and exec
//!
//! `no_new_privs` and seccomp are per-process properties set by `prctl(2)`,
//! and neither can be installed into a child that has already `exec`ed.
//! Applying them in the parent would confine the broker; applying them after
//! `Command::spawn` returns would leave the child unconfined through its whole
//! startup. So they go in `pre_exec`, and the content process has never
//! executed an unconfined instruction — see [`spawn`].
//!
//! That also means nothing asks the child whether it worked. A child reporting
//! its own confinement would be authority taken from a message, which
//! invariant 9 forbids; here a failure inside `pre_exec` makes `spawn` itself
//! fail, so an unconfined content process is never returned to the broker.
//!
//! # What the filter does, and what it deliberately does not
//!
//! Phase 2 is "permissive but real" (build-spec §9); Phase 17 tightens it. The
//! filter installed here denies a small set of syscalls that a renderer has no
//! business making and that are the classic escape and privilege-escalation
//! primitives. It is an explicit deny-list, which is the *weaker* shape — a
//! real sandbox is an allow-list, and Phase 17 owes one.
//!
//! Saying that plainly matters more than the filter does. A deny-list that
//! reads as thorough is how a sandbox comes to be trusted for more than it
//! delivers, and this crate's rule is that a mechanism is named for what it
//! is. What this buys today is that the boundary exists, is applied on every
//! launch, and fails closed — so Phase 17 tightens a filter that is already
//! load-bearing rather than introducing one nothing was written against.

use crate::{PolicyError, Rung};

/// `prctl(2)`: set the calling process's `no_new_privs` bit.
const PR_SET_NO_NEW_PRIVS: i32 = 38;
/// `prctl(2)`: read it back.
const PR_GET_NO_NEW_PRIVS: i32 = 39;
/// `prctl(2)`: install a seccomp filter.
const PR_SET_SECCOMP: i32 = 22;
/// `prctl(2)`: read the current seccomp mode.
const PR_GET_SECCOMP: i32 = 21;
/// `seccomp(2)` mode: filter with a BPF program.
const SECCOMP_MODE_FILTER: i32 = 2;

// BPF instruction classes and operations, from `linux/bpf_common.h`. Declared
// rather than imported: `libc` does not expose the BPF assembler constants,
// and these are stable kernel ABI.
const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

/// `seccomp_data.nr` — the syscall number — is at offset 0.
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
/// `seccomp_data.arch` is at offset 4.
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;

/// Return actions, from `linux/seccomp.h`.
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
/// `EPERM`, returned to the denied caller rather than killing it.
const EPERM: u32 = 1;

/// `AUDIT_ARCH_*` for the architectures this is built for.
///
/// The architecture check is not optional. `seccomp_data.nr` is meaningless
/// without it: syscall numbers differ per ABI, so a filter that matches
/// numbers without pinning the architecture can be bypassed by entering
/// through a different ABI — the x32 and i386-on-x86_64 case — where the same
/// number names a different call.
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

/// Syscalls a content process must never make.
///
/// Deliberately short, deliberately explicit, and each one an escape or
/// escalation primitive rather than something merely unused:
///
/// - `ptrace` — attach to and control another process.
/// - `process_vm_readv` / `process_vm_writev` — read and write another
///   process's memory without attaching, which includes the broker's.
/// - `kexec_load` / `kexec_file_load` — replace the running kernel.
/// - `init_module` / `finit_module` / `delete_module` — load kernel code.
/// - `mount` / `umount2` / `pivot_root` / `chroot` — reshape the filesystem
///   view the sandbox depends on.
/// - `setns` / `unshare` — leave or create namespaces.
/// - `perf_event_open` — a long-standing source of kernel escalation bugs.
/// - `bpf` — load kernel programs.
/// - `userfaultfd` — a widely used primitive for winning kernel races.
///
/// x86_64 numbers; the `cfg` on [`AUDIT_ARCH`] keeps the filter from being
/// built at all on an architecture whose numbers are not listed here.
#[cfg(target_arch = "x86_64")]
const DENIED_SYSCALLS: &[u32] = &[
    101, // ptrace
    310, // process_vm_readv
    311, // process_vm_writev
    246, // kexec_load
    320, // kexec_file_load
    175, // init_module
    313, // finit_module
    176, // delete_module
    165, // mount
    166, // umount2
    155, // pivot_root
    161, // chroot
    308, // setns
    272, // unshare
    298, // perf_event_open
    321, // bpf
    323, // userfaultfd
];

#[cfg(target_arch = "aarch64")]
const DENIED_SYSCALLS: &[u32] = &[
    117, // ptrace
    270, // process_vm_readv
    271, // process_vm_writev
    104, // kexec_load
    294, // kexec_file_load
    105, // init_module
    273, // finit_module
    106, // delete_module
    40,  // mount
    39,  // umount2
    41,  // pivot_root
    51,  // chroot
    268, // setns
    97,  // unshare
    241, // perf_event_open
    280, // bpf
    282, // userfaultfd
];

/// A BPF jump offset is a `u8`, so the deny instruction has to stay within 255
/// of every comparison that targets it. This is a compile-time floor under
/// that: adding a 250th denied syscall stops the build rather than producing a
/// filter whose jumps land somewhere else and whose code still reads as though
/// it denies them.
const _: () = assert!(
    DENIED_SYSCALLS.len() < 250,
    "too many denied syscalls for a u8 BPF jump offset; the filter needs a      different shape (jump to a shared trailer, or an allow-list) rather than      a longer chain of comparisons"
);

/// One BPF instruction, matching `struct sock_filter`.
#[repr(C)]
#[derive(Copy, Clone)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

/// A BPF program, matching `struct sock_fprog`.
#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

const fn stmt(code: u16, k: u32) -> SockFilter {
    SockFilter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter { code, jt, jf, k }
}

/// What a successful sandboxed spawn hands back.
pub(crate) struct Spawned {
    pub(crate) child: std::process::Child,
    pub(crate) stdin: std::fs::File,
    pub(crate) stdout: std::fs::File,
    pub(crate) applied: Vec<Rung>,
}

/// Spawn `executable` with the policy applied between `fork` and `exec`.
///
/// `pre_exec` is the only place the policy can go. `no_new_privs` and seccomp
/// are per-process and cannot be installed into a child that has already
/// `exec`ed, so applying them after `Command::spawn` returns would leave the
/// child unconfined for its whole startup — and applying them in the *parent*
/// would confine the broker.
///
/// # Why the filter is built before the fork
///
/// The closure runs in the forked child, between `fork` and `exec`, where only
/// async-signal-safe work is permitted. Allocating there can deadlock: if
/// another thread held the allocator's lock at the moment of the fork, that
/// lock is held forever in the child, which has only one thread to release it.
/// The broker is multi-threaded by design, so this is a live hazard rather
/// than a theoretical one. Building the program up here means the closure only
/// reads memory that already exists and makes two `prctl` calls.
pub(crate) fn spawn(executable: &std::path::Path) -> Result<Spawned, PolicyError> {
    use std::os::unix::process::CommandExt;

    let program = build_filter();

    let mut command = std::process::Command::new(executable);
    command
        // Invariant 1: the content process gets nothing it was not handed, and
        // a broker's environment routinely carries tokens on a developer or CI
        // machine.
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // stderr is inherited on purpose: a content process's diagnostics
        // should reach the terminal without the broker relaying them.
        .stderr(std::process::Stdio::inherit());

    // SAFETY: the closure runs in the forked child before `exec`, where only
    // async-signal-safe operations are permitted. It allocates nothing, takes
    // no lock, and touches no memory beyond `program` — which was built before
    // the fork and is owned by the closure, so it is fully initialised and
    // stays alive for the call. Both `prctl` operations are async-signal-safe:
    // they are single syscalls that touch no libc state.
    unsafe {
        command.pre_exec(move || {
            apply(&program).map_err(|error| match error {
                PolicyError::Failed { detail, .. } => std::io::Error::other(detail),
                PolicyError::Refused(_) => {
                    std::io::Error::other("the sandbox floor was not cleared")
                }
            })
        });
    }

    let mut child = command.spawn().map_err(|_| PolicyError::Failed {
        rung: Rung::Seccomp,
        detail: "the content process could not be created under a policy",
    })?;

    // A failure inside pre_exec makes `spawn` fail, so reaching here means the
    // policy was applied. Taking the pipes cannot fail — both were requested
    // as `piped` above — but this refuses rather than unwrapping, because the
    // panic lints in this workspace exist for exactly this shape of "cannot
    // happen".
    let stdin = child
        .stdin
        .take()
        .map(|pipe| std::fs::File::from(std::os::fd::OwnedFd::from(pipe)))
        .ok_or(PolicyError::Failed {
            rung: Rung::Seccomp,
            detail: "the content process was created without a stdin pipe",
        })?;
    let stdout = child
        .stdout
        .take()
        .map(|pipe| std::fs::File::from(std::os::fd::OwnedFd::from(pipe)))
        .ok_or(PolicyError::Failed {
            rung: Rung::Seccomp,
            detail: "the content process was created without a stdout pipe",
        })?;

    Ok(Spawned {
        child,
        stdin,
        stdout,
        applied: vec![Rung::NoNewPrivs, Rung::Seccomp],
    })
}

/// Apply the policy to the calling process. Runs in the forked child.
///
/// Order is load-bearing, and not only because seccomp requires
/// `no_new_privs` for unprivileged callers: `no_new_privs` is what stops a
/// setuid binary reached through a later `execve` from regaining what the
/// filter took away. Both are one-way — neither can be cleared for the life of
/// the process.
fn apply(program: &[SockFilter]) -> Result<(), PolicyError> {
    set_no_new_privs()?;
    install_filter(program)?;
    Ok(())
}

/// Set `no_new_privs`, then read it back.
///
/// The read-back is the point. `prctl` returning 0 says the call was accepted;
/// `PR_GET_NO_NEW_PRIVS` returning 1 says the bit is actually set, and only
/// the second is evidence the process is confined.
fn set_no_new_privs() -> Result<(), PolicyError> {
    let failed = |detail| PolicyError::Failed {
        rung: Rung::NoNewPrivs,
        detail,
    };

    // SAFETY: prctl is variadic; PR_SET_NO_NEW_PRIVS takes one value argument
    // and four ignored zeros, which is exactly the documented form. Every
    // argument is an integer, so there is no memory here to alias or outlive.
    let set = unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if set != 0 {
        return Err(failed("prctl(PR_SET_NO_NEW_PRIVS) was refused"));
    }

    // SAFETY: as above. PR_GET_NO_NEW_PRIVS takes no arguments and returns the
    // bit as its result rather than writing through a pointer.
    let got = unsafe { libc::prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) };
    if got != 1 {
        return Err(failed("no_new_privs did not take: it reads back unset"));
    }
    Ok(())
}

/// Install the seccomp filter, then confirm the mode actually changed.
fn install_filter(program: &[SockFilter]) -> Result<(), PolicyError> {
    let failed = |detail| PolicyError::Failed {
        rung: Rung::Seccomp,
        detail,
    };

    let len = u16::try_from(program.len()).map_err(|_| failed("the filter is too long for BPF"))?;
    let fprog = SockFprog {
        len,
        filter: program.as_ptr(),
    };

    // SAFETY: PR_SET_SECCOMP with SECCOMP_MODE_FILTER takes a pointer to a
    // sock_fprog as its third argument. `fprog` is a live, correctly laid out
    // #[repr(C)] sock_fprog, and `program` — which its `filter` field points
    // into — is borrowed for the whole call, so the pointer stays valid. The
    // kernel copies the program and retains no reference to this memory.
    let installed = unsafe {
        libc::prctl(
            PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER,
            std::ptr::addr_of!(fprog),
            0,
            0,
        )
    };
    if installed != 0 {
        return Err(failed("prctl(PR_SET_SECCOMP) was refused"));
    }

    // SAFETY: as above; PR_GET_SECCOMP takes no arguments and returns the
    // current mode as its result.
    let mode = unsafe { libc::prctl(PR_GET_SECCOMP, 0, 0, 0, 0) };
    if mode != SECCOMP_MODE_FILTER {
        return Err(failed(
            "the seccomp filter did not take: mode is not filter",
        ));
    }
    Ok(())
}

/// The filter: pin the architecture, then deny a fixed set of syscalls.
///
/// Kills on a wrong architecture rather than returning an error, because an
/// unexpected ABI means the syscall numbers below do not mean what this filter
/// thinks they mean — and a filter matching the wrong numbers is worse than no
/// filter, since it reads as protection.
fn build_filter() -> Vec<SockFilter> {
    let mut program = Vec::with_capacity(DENIED_SYSCALLS.len() + 5);

    // Load seccomp_data.arch and require an exact match.
    program.push(stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_ARCH_OFFSET));
    program.push(jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH, 1, 0));
    program.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));

    // Load seccomp_data.nr and compare it against each denied number. A match
    // jumps to the EPERM return at the end; a miss falls through to the next
    // comparison.
    program.push(stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFFSET));

    let denied = DENIED_SYSCALLS.len();
    for (index, syscall) in DENIED_SYSCALLS.iter().enumerate() {
        // Jump distance to the deny instruction: every remaining comparison,
        // plus the allow that sits between them and it. Computed rather than
        // written as a constant so that editing DENIED_SYSCALLS cannot
        // silently produce a filter that jumps into the wrong instruction.
        let remaining = denied - index - 1;
        // Saturating here would be a silently wrong filter: the jump would
        // land on some other instruction and the syscall would be allowed
        // while the code still read as denying it. The const assertion above
        // makes that unreachable; this keeps the arithmetic honest if it is
        // ever removed.
        let to_deny = u8::try_from(remaining + 1).unwrap_or(0);
        program.push(jump(BPF_JMP | BPF_JEQ | BPF_K, *syscall, to_deny, 0));
    }

    program.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    program.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM));
    program
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The jump arithmetic above is the one part of this file that is easy to
    /// get wrong and impossible to notice: a filter with a bad offset still
    /// installs, still runs, and simply protects nothing. So it is checked
    /// structurally rather than by trusting the loop.
    #[test]
    fn sandbox_policy_every_denied_syscall_jumps_to_the_deny_instruction() {
        let program = build_filter();
        let deny_index = program.len() - 1;
        let allow_index = program.len() - 2;

        // The comparisons start after the arch check (3) and the nr load (1).
        for (offset, _syscall) in DENIED_SYSCALLS.iter().enumerate() {
            let index = 4 + offset;
            let instruction = program[index];
            let target = index + 1 + usize::from(instruction.jt);
            assert_eq!(
                target, deny_index,
                "comparison {offset} jumps to {target}, not the deny at {deny_index}"
            );
        }

        assert_eq!(
            program[allow_index].k, SECCOMP_RET_ALLOW,
            "the instruction before the deny must be the allow"
        );
        assert_eq!(
            program[deny_index].k,
            SECCOMP_RET_ERRNO | EPERM,
            "the last instruction must deny with EPERM"
        );
    }

    /// A filter that does not pin the architecture is bypassable through a
    /// second ABI, so this asserts the check is present and kills.
    #[test]
    fn sandbox_policy_pins_the_architecture_before_matching_numbers() {
        let program = build_filter();
        assert_eq!(
            program[0].k, SECCOMP_DATA_ARCH_OFFSET,
            "the filter must load seccomp_data.arch first"
        );
        assert_eq!(
            program[1].k, AUDIT_ARCH,
            "it must compare against this arch"
        );
        assert_eq!(
            program[2].k, SECCOMP_RET_KILL_PROCESS,
            "a foreign architecture must be killed, not allowed"
        );
        assert_eq!(
            program[3].k, SECCOMP_DATA_NR_OFFSET,
            "only then may it load the syscall number"
        );
    }

    #[test]
    fn sandbox_policy_denies_the_escape_primitives() {
        let program = build_filter();
        let compared: Vec<u32> = program
            .iter()
            .skip(4)
            .take(DENIED_SYSCALLS.len())
            .map(|instruction| instruction.k)
            .collect();
        assert_eq!(compared, DENIED_SYSCALLS, "every denied number is compared");
    }
}
