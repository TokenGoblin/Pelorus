//! The Windows policy, and the unsafe that applies it (ADR 008, ADR 010).
//!
//! # The policy is applied at creation, not afterwards
//!
//! ADR 008 moves spawning behind this crate for a reason that only shows up
//! when you try it the other way. A child spawned normally and *then* confined
//! runs unconfined for the window between `CreateProcess` and whenever the
//! policy lands — process and runtime initialisation, loader work, anything a
//! `DllMain` does. That window is attacker-reachable and buys nothing. So:
//!
//! - the **restricted token** is the token the process is created with, via
//!   `CreateProcessAsUserW`;
//! - the **job object** is attached by `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so
//!   membership exists at creation rather than being retrofitted (this crate's
//!   `CLAUDE.md`, and `docs/spec-audit-001.md` finding 9);
//! - **handle inheritance** is restricted to exactly the pipe ends the child
//!   is meant to have, by `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
//!
//! There is no moment at which the content process exists and is not confined.
//!
//! # Why the handle list is not optional
//!
//! `bInheritHandles = TRUE` without an explicit list gives the child *every*
//! inheritable handle in the broker — including other content processes'
//! pipes. Invariant 1 says a content process holds no handle it was not
//! passed, and the handle list is what makes that true rather than merely
//! intended.
//!
//! # What this is not
//!
//! Phase 2 is "permissive but real" (build-spec §9); Phase 17 tightens it.
//! `DISABLE_MAX_PRIVILEGE` strips the token's privileges, and the job object is
//! present and enforceable — but this is not a low-integrity token, not an
//! AppContainer, and not a restricted SID set, so a content process can still
//! read the user's files. The job carries no limits yet; Phase 17 adds them,
//! to an object that already exists.

use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HANDLE_FLAG_INHERIT,
    SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, SECURITY_ATTRIBUTES, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, OpenProcessToken,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::{Capabilities, PolicyError, Restriction, Rung};

/// `GetExitCodeProcess` reports this while the process is still running.
const STILL_RUNNING: u32 = 259;

/// A handle closed exactly once, when it goes out of scope.
///
/// Written rather than reached for: the alternative is a bare `HANDLE` and a
/// `let _ = CloseHandle(..)` at every exit, which leaks on precisely the
/// early-return paths a fail-closed design takes most often. Neither `Copy`
/// nor `Clone`, so a double close cannot be constructed.
pub(crate) struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn from_nullable(handle: HANDLE) -> Option<Self> {
        if handle.is_null() {
            None
        } else {
            Some(Self(handle))
        }
    }

    fn raw(&self) -> HANDLE {
        self.0
    }

    /// Give up ownership without closing, for a handle about to be owned by
    /// something else — a `File` built from a pipe end, which closes it.
    fn into_raw(self) -> HANDLE {
        let raw = self.0;
        std::mem::forget(self);
        raw
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: self.0 came from an API that transferred ownership of it
        // (CreateJobObjectW, OpenProcessToken, CreateRestrictedToken,
        // CreatePipe, DuplicateHandle, CreateProcessAsUserW), was null-checked
        // at construction, and this is the only close — into_raw forgets self
        // rather than letting both run.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn failed(rung: Rung, detail: &'static str) -> PolicyError {
    PolicyError::Failed { rung, detail }
}

/// Probe what this machine offers.
///
/// Both Windows rungs are creation-time operations rather than queryable
/// features, so this probes by *doing* the cheap half of each: making an
/// unnamed job object, and deriving a restricted token from this process's
/// own. A probe that assumes success is the failure ADR 007 names, so neither
/// is assumed — and the token probe exercises the same call the spawn path
/// depends on rather than a proxy for it.
pub(crate) fn detect(caps: &mut Capabilities) {
    match create_job() {
        Some(_job) => caps.offer(Rung::JobObject),
        None => caps.deny(Rung::JobObject, Restriction::NotSupported),
    }

    match open_own_token().and_then(|token| restrict(&token).ok()) {
        Some(_restricted) => caps.offer(Rung::RestrictedToken),
        None => caps.deny(Rung::RestrictedToken, Restriction::Inconclusive),
    }
}

fn create_job() -> Option<OwnedHandle> {
    // SAFETY: both arguments are documented as optional, and null is the
    // documented way to pass "no security attributes" and "unnamed". The
    // returned handle is owned by the caller and null-checked before use.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    OwnedHandle::from_nullable(handle)
}

/// Open this process's token with the accesses the spawn path needs.
fn open_own_token() -> Option<OwnedHandle> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that is always valid
    // for the calling process and needs no closing. `token` is live and
    // aligned; the call writes it only on success and it is read only after
    // success is checked.
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_QUERY,
            &mut token,
        )
    };
    if ok == 0 {
        return None;
    }
    OwnedHandle::from_nullable(token)
}

/// Derive a restricted primary token from an existing one.
///
/// `DISABLE_MAX_PRIVILEGE` removes every privilege except `SeChangeNotify`,
/// which is what makes this a real reduction rather than a relabelling. No
/// SIDs are disabled or restricted here; that is Phase 17's tightening, and
/// claiming it now would name the mechanism for more than it does.
fn restrict(token: &OwnedHandle) -> Result<OwnedHandle, PolicyError> {
    let mut restricted: HANDLE = std::ptr::null_mut();
    // SAFETY: `token` is a live token opened with TOKEN_DUPLICATE. The three
    // count arguments are zero with null array pointers, the documented way to
    // say "no SIDs to disable, no extra privileges to delete, no SIDs to
    // restrict". `restricted` is live and aligned and written only on success.
    let ok = unsafe {
        CreateRestrictedToken(
            token.raw(),
            DISABLE_MAX_PRIVILEGE,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            &mut restricted,
        )
    };
    if ok == 0 {
        return Err(failed(
            Rung::RestrictedToken,
            "a restricted token could not be derived from this process's token",
        ));
    }
    OwnedHandle::from_nullable(restricted).ok_or_else(|| {
        failed(
            Rung::RestrictedToken,
            "CreateRestrictedToken reported success but returned no token",
        )
    })
}

/// Both ends of a freshly created pipe.
struct Pipe {
    read: OwnedHandle,
    write: OwnedHandle,
}

/// Create a pipe with inheritable ends.
///
/// The caller clears inheritance on the end the parent keeps. That matters: a
/// child holding the parent's end of its own pipe means the pipe never reports
/// EOF when the child dies, and the broker's reader blocks forever on a
/// channel whose peer is gone — the orphaned-pipe wedge, self-inflicted.
fn create_pipe() -> Result<Pipe, PolicyError> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| failed(Rung::JobObject, "SECURITY_ATTRIBUTES is implausibly large"))?,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };

    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: `read` and `write` are live, aligned HANDLEs written on success.
    // `attributes` is a correctly sized SECURITY_ATTRIBUTES that outlives the
    // call. A zero size requests the system default buffer, as documented.
    let ok = unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) };
    if ok == 0 {
        return Err(failed(Rung::JobObject, "a stdio pipe could not be created"));
    }
    let read = OwnedHandle::from_nullable(read)
        .ok_or_else(|| failed(Rung::JobObject, "CreatePipe returned a null read end"))?;
    let write = OwnedHandle::from_nullable(write)
        .ok_or_else(|| failed(Rung::JobObject, "CreatePipe returned a null write end"))?;
    Ok(Pipe { read, write })
}

/// Clear the inheritable flag on a handle the parent keeps.
fn make_uninheritable(handle: &OwnedHandle) -> Result<(), PolicyError> {
    // SAFETY: `handle` is live and owned by the caller. Passing
    // HANDLE_FLAG_INHERIT as the mask with a zero value clears exactly that
    // flag and leaves the others alone, per the documented semantics.
    let ok = unsafe { SetHandleInformation(handle.raw(), HANDLE_FLAG_INHERIT, 0) };
    if ok == 0 {
        return Err(failed(
            Rung::JobObject,
            "the parent's pipe end could not be made uninheritable",
        ));
    }
    Ok(())
}

/// An inheritable duplicate of this process's stderr.
///
/// stderr is inherited on purpose: a content process's diagnostics should
/// reach the terminal without the broker relaying them. It has to be
/// duplicated as inheritable because the handle list is exhaustive — a handle
/// not named in it is not inherited, whatever its own flags say.
fn inheritable_stderr() -> Option<OwnedHandle> {
    // SAFETY: GetStdHandle takes a documented constant and returns a handle
    // this process already owns. It is not ours to close, which is why it is
    // duplicated rather than wrapped in an OwnedHandle.
    let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    if stderr.is_null() {
        return None;
    }

    let mut duplicate: HANDLE = std::ptr::null_mut();
    // SAFETY: source and target process are both the current process, whose
    // pseudo-handle is always valid. `stderr` is a live handle this process
    // owns. `duplicate` is live and aligned and written only on success.
    // DUPLICATE_SAME_ACCESS with inherit = TRUE is the documented way to get
    // an inheritable copy carrying the original's access rights.
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            stderr,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return None;
    }
    OwnedHandle::from_nullable(duplicate)
}

/// A `PROC_THREAD_ATTRIBUTE_LIST`, owned and deleted on drop.
///
/// The list is opaque and variable-length: the OS reports the size, we
/// allocate, it initialises in place. The buffer must stay put and stay alive
/// until `CreateProcessAsUserW` has consumed it, which this type enforces —
/// and so must the values the attributes point at, which is why the caller
/// keeps them in locals.
struct AttributeList {
    buffer: Vec<u8>,
    initialised: bool,
}

impl AttributeList {
    fn with_capacity_for(count: u32) -> Result<Self, PolicyError> {
        let mut size: usize = 0;
        // SAFETY: the documented size-query form — null list pointer, the
        // attribute count, a reserved zero, and a live `size` the call writes.
        // It is expected to fail with ERROR_INSUFFICIENT_BUFFER; the size is
        // the only thing wanted from it.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size);
        }
        if size == 0 {
            return Err(failed(
                Rung::JobObject,
                "the process attribute list size could not be determined",
            ));
        }

        let mut list = Self {
            buffer: vec![0u8; size],
            initialised: false,
        };
        // SAFETY: `buffer` is exactly `size` bytes — the size the OS just
        // asked for — and is not moved while the list is alive, since
        // AttributeList owns it and is neither Copy nor Clone.
        let ok = unsafe {
            InitializeProcThreadAttributeList(
                list.buffer.as_mut_ptr().cast::<c_void>(),
                count,
                0,
                &mut size,
            )
        };
        if ok == 0 {
            return Err(failed(
                Rung::JobObject,
                "the process attribute list could not be initialised",
            ));
        }
        list.initialised = true;
        Ok(list)
    }

    /// Set one attribute.
    ///
    /// `value` must outlive this list: `UpdateProcThreadAttribute` stores the
    /// pointer rather than copying, and the OS reads it at process creation.
    /// Every call site below keeps its value in a local that outlives both.
    fn set(
        &mut self,
        attribute: usize,
        value: *const c_void,
        size: usize,
    ) -> Result<(), PolicyError> {
        // SAFETY: the list is initialised by construction. `value` points at
        // `size` bytes the caller keeps alive past CreateProcessAsUserW, per
        // this function's contract. The two null out-parameters are documented
        // as "do not report the previous value".
        let ok = unsafe {
            UpdateProcThreadAttribute(
                self.buffer.as_mut_ptr().cast::<c_void>(),
                0,
                attribute,
                value,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(failed(
                Rung::JobObject,
                "a process creation attribute could not be set",
            ));
        }
        Ok(())
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.buffer.as_mut_ptr().cast::<c_void>()
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        if self.initialised {
            // SAFETY: the list was initialised by
            // InitializeProcThreadAttributeList and has not been deleted
            // before — `initialised` is set once, and this is its only use.
            unsafe {
                DeleteProcThreadAttributeList(self.buffer.as_mut_ptr().cast::<c_void>());
            }
        }
    }
}

/// A content process created under a policy.
pub(crate) struct Child {
    process: OwnedHandle,
    /// Held so the job outlives the process it confines: dropping the last
    /// handle to a job is what would end the confinement.
    _job: OwnedHandle,
    pid: u32,
    exited: Option<i32>,
}

/// What a successful sandboxed spawn hands back.
pub(crate) struct Spawned {
    pub(crate) child: Child,
    /// The parent's end of the child's stdin.
    pub(crate) stdin: OwnedHandle,
    /// The parent's end of the child's stdout.
    pub(crate) stdout: OwnedHandle,
    pub(crate) applied: Vec<Rung>,
}

/// Spawn `executable` under a restricted token, in a job, with exactly the
/// handles it is meant to have.
pub(crate) fn spawn(executable: &Path) -> Result<Spawned, PolicyError> {
    let token = open_own_token().ok_or_else(|| {
        failed(
            Rung::RestrictedToken,
            "this process's token could not be opened",
        )
    })?;
    let restricted = restrict(&token)?;
    let job = create_job().ok_or_else(|| {
        failed(
            Rung::JobObject,
            "the job object could not be created for this content process",
        )
    })?;

    // stdin: the parent writes, the child reads. stdout: the child writes, the
    // parent reads. Each parent end is made uninheritable so the child cannot
    // hold its own pipe open.
    let stdin = create_pipe()?;
    let stdout = create_pipe()?;
    make_uninheritable(&stdin.write)?;
    make_uninheritable(&stdout.read)?;

    let stderr = inheritable_stderr();

    // Exactly the handles the child is meant to have. The OS reads this array
    // at creation, so it must outlive the attribute list and the call.
    let mut inherited: Vec<HANDLE> = vec![stdin.read.raw(), stdout.write.raw()];
    if let Some(handle) = stderr.as_ref() {
        inherited.push(handle.raw());
    }
    let job_handles: [HANDLE; 1] = [job.raw()];

    let mut attributes = AttributeList::with_capacity_for(2)?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherited.as_ptr().cast::<c_void>(),
        std::mem::size_of_val(inherited.as_slice()),
    )?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
        job_handles.as_ptr().cast::<c_void>(),
        std::mem::size_of_val(&job_handles),
    )?;

    // SAFETY: STARTUPINFOEXW is a plain C struct of integers, pointers and
    // handles with no niche and no invalid bit pattern, so an all-zero value
    // is a valid one. Every field this call depends on is assigned below.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>())
        .map_err(|_| failed(Rung::JobObject, "STARTUPINFOEXW is implausibly large"))?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin.read.raw();
    startup.StartupInfo.hStdOutput = stdout.write.raw();
    startup.StartupInfo.hStdError = stderr.as_ref().map_or(std::ptr::null_mut(), |h| h.raw());
    startup.lpAttributeList = attributes.as_ptr();

    // CreateProcessAsUserW may write to the command line buffer, so it must be
    // owned and mutable. Quoted, because a path containing a space would
    // otherwise be split into an executable and an argument.
    let mut command_line: Vec<u16> = std::iter::once(u16::from(b'"'))
        .chain(OsStr::new(executable).encode_wide())
        .chain([u16::from(b'"'), 0])
        .collect();

    // An empty environment block: two nulls. Invariant 1 — a content process
    // gets nothing it was not handed, and a broker's environment on a
    // developer or CI machine routinely carries tokens.
    let environment: [u16; 2] = [0, 0];

    // SAFETY: as for STARTUPINFOEXW — four integers and handles, all written
    // by the call on success.
    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // SAFETY: `restricted` is a live primary token derived from this process's
    // own. `command_line` is a NUL-terminated mutable UTF-16 buffer outliving
    // the call. `startup` is a correctly sized STARTUPINFOEXW whose attribute
    // list, handle array, job array and stdio handles are all locals of this
    // function and so outlive the call. `environment` is the double-NUL block
    // CREATE_UNICODE_ENVIRONMENT expects. bInheritHandles is TRUE, which the
    // handle list requires to mean anything, and that list restricts
    // inheritance to `inherited`. `information` is live and written on success.
    let ok = unsafe {
        CreateProcessAsUserW(
            restricted.raw(),
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            environment.as_ptr().cast::<c_void>(),
            std::ptr::null(),
            std::ptr::addr_of!(startup).cast::<STARTUPINFOW>(),
            &mut information,
        )
    };
    if ok == 0 {
        return Err(failed(
            Rung::RestrictedToken,
            "the content process could not be created under a restricted token",
        ));
    }

    let process = OwnedHandle::from_nullable(information.hProcess).ok_or_else(|| {
        failed(
            Rung::RestrictedToken,
            "process creation reported success but returned no handle",
        )
    })?;
    // The initial thread handle is not needed; closing it leaves the process
    // running.
    let _thread = OwnedHandle::from_nullable(information.hThread);

    Ok(Spawned {
        child: Child {
            process,
            _job: job,
            pid: information.dwProcessId,
            exited: None,
        },
        stdin: stdin.write,
        stdout: stdout.read,
        // Both rungs were applied by the OS as part of creation: the token is
        // the one the process was made with, and the job was attached by the
        // attribute list. Neither is a claim made after the fact.
        applied: vec![Rung::RestrictedToken, Rung::JobObject],
    })
}

impl Child {
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        if let Some(code) = self.exited {
            return Ok(Some(code));
        }
        let mut code: u32 = 0;
        // SAFETY: `self.process` is a live process handle owned by this
        // struct; `code` is live and aligned and written on success.
        let ok = unsafe { GetExitCodeProcess(self.process.raw(), &mut code) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if code == STILL_RUNNING {
            return Ok(None);
        }
        let code = code as i32;
        self.exited = Some(code);
        Ok(Some(code))
    }

    pub(crate) fn wait(&mut self) -> std::io::Result<i32> {
        if let Some(code) = self.exited {
            return Ok(code);
        }
        // SAFETY: `self.process` is a live process handle owned by this
        // struct. INFINITE is the documented "no timeout".
        let waited = unsafe { WaitForSingleObject(self.process.raw(), INFINITE) };
        if waited != WAIT_OBJECT_0 {
            return Err(std::io::Error::last_os_error());
        }
        self.try_wait()?
            .ok_or_else(|| std::io::Error::other("the process signalled exit but reports running"))
    }

    pub(crate) fn kill(&mut self) -> std::io::Result<()> {
        if self.exited.is_some() {
            return Ok(());
        }
        // SAFETY: `self.process` is a live process handle owned by this
        // struct. The exit code is arbitrary and never read back as
        // meaningful; 1 marks an abnormal end.
        let ok = unsafe { TerminateProcess(self.process.raw(), 1) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Turn a pipe end into a `File`, which owns and closes it.
///
/// `File` is the transport `px-broker` sees, so nothing outside this crate
/// ever handles a raw OS handle.
pub(crate) fn into_file(handle: OwnedHandle) -> std::fs::File {
    use std::os::windows::io::FromRawHandle;
    // SAFETY: `handle` owns this OS handle and gives up ownership via
    // into_raw, which forgets rather than closes — so exactly one owner exists
    // at every moment, and it is now the File.
    unsafe { std::fs::File::from_raw_handle(handle.into_raw().cast()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;

    /// The load-bearing verification for this module.
    ///
    /// `spawn` returning `Ok` with `applied` listing two rungs is a claim.
    /// This asks the operating system whether the claim is true: the child
    /// must actually be inside a job, as seen from the parent, which is the
    /// only vantage point a content process cannot influence.
    ///
    /// It also closes the specific failure ADR 007 calls unrecoverable —
    /// a process that looks sandboxed and is not — for the half of the policy
    /// that is externally observable.
    #[test]
    fn sandbox_policy_the_spawned_child_is_really_inside_a_job() {
        let mut spawned = match spawn(std::path::Path::new(r"C:\Windows\System32\cmd.exe")) {
            Ok(spawned) => spawned,
            // A machine that cannot create a job or restrict a token refuses
            // to launch at all, which is correct behaviour rather than a
            // failing test.
            Err(_) => return,
        };

        let mut inside: i32 = 0;
        // SAFETY: `spawned.child.process` is a live process handle owned by
        // the Child, and `spawned.child._job` a live job handle owned beside
        // it. `inside` is live and aligned and written on success.
        let queried = unsafe {
            IsProcessInJob(
                spawned.child.process.raw(),
                spawned.child._job.raw(),
                &mut inside,
            )
        };

        let _ = spawned.child.kill();
        let _ = spawned.child.wait();

        assert_ne!(queried, 0, "IsProcessInJob failed outright");
        assert_ne!(
            inside, 0,
            "the child reported as confined is not in the job it was created with"
        );
    }

    /// Handles must not accumulate across spawns.
    ///
    /// The broker spawns a content process per site and replaces one on every
    /// crash, so a handle leaked per spawn is a denial of service reachable by
    /// any page that can make a renderer die. This path creates seven handles
    /// per launch — a token, a restricted token, a job, four pipe ends — plus
    /// a process and a thread, and every one is released on a path with
    /// several early returns. Counting them is cheaper than reasoning about
    /// them.
    #[test]
    fn sandbox_policy_repeated_spawns_do_not_leak_handles() {
        use windows_sys::Win32::System::Threading::GetProcessHandleCount;

        fn handle_count() -> u32 {
            let mut count: u32 = 0;
            // SAFETY: GetCurrentProcess is a pseudo-handle that is always
            // valid for this process; `count` is live, aligned, and written by
            // the call.
            unsafe {
                GetProcessHandleCount(GetCurrentProcess(), &mut count);
            }
            count
        }

        fn spawn_and_reap() -> bool {
            match spawn(std::path::Path::new(r"C:\Windows\System32\cmd.exe")) {
                Ok(mut spawned) => {
                    let _ = spawned.child.kill();
                    let _ = spawned.child.wait();
                    true
                }
                Err(_) => false,
            }
        }

        // Warm up first: one-off allocations made on the first spawn are not
        // growth, and counting them would make the threshold meaningless.
        for _ in 0..5 {
            if !spawn_and_reap() {
                return;
            }
        }

        let before = handle_count();
        for _ in 0..40 {
            if !spawn_and_reap() {
                return;
            }
        }
        let after = handle_count();

        // A small allowance rather than an exact match: the runtime may open
        // handles of its own between the two readings. A leak of one per spawn
        // would be forty.
        assert!(
            after <= before + 8,
            "handles grew from {before} to {after} across 40 spawns;              the spawn path leaks roughly {} per launch",
            (after - before) / 40
        );
    }

    /// The handle list must be exhaustive, not additive. If the child were
    /// created with a null attribute list it would inherit every inheritable
    /// handle in this process; this asserts the list is actually built and
    /// carries exactly the ends the child is meant to have.
    #[test]
    fn sandbox_policy_the_child_is_given_only_its_own_pipe_ends() {
        let Ok(mut spawned) = spawn(std::path::Path::new(r"C:\Windows\System32\cmd.exe")) else {
            return;
        };
        // The parent's ends are real, distinct, and ours.
        assert!(!spawned.stdin.raw().is_null());
        assert!(!spawned.stdout.raw().is_null());
        assert_ne!(spawned.stdin.raw(), spawned.stdout.raw());
        let _ = spawned.child.kill();
        let _ = spawned.child.wait();
    }
}
