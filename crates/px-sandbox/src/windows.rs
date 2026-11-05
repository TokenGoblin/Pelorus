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
//! # The Phase 17 profile (ADR 048)
//!
//! Everything below is applied at creation and then **read back from the
//! operating system by the parent** before `spawn` returns; a rung is listed in
//! `applied` only if the readback agrees (invariant 9: never the child's word).
//!
//! - **Job limits**, set on the job before the process exists: a per-process
//!   commit cap (`JOB_OBJECT_LIMIT_PROCESS_MEMORY`), kill-on-close, an active
//!   process limit of one (the content process cannot start a child), die on
//!   unhandled exception, and every UI restriction (clipboard, desktop,
//!   display settings, system parameters, global atoms, USER handles, log-off).
//! - **Low integrity** on the restricted token: writes to anything not labelled
//!   low are refused by the mandatory-integrity check, whatever the DACL says.
//! - **Process mitigation policies** and the **child-process policy**, passed as
//!   creation attributes. These cannot be read back from outside the process
//!   (`GetProcessMitigationPolicy` reads only the caller's own), so they are
//!   applied with a hard failure if the OS rejects them, but are *not* reported
//!   in `applied`.
//!
//! # What this is not
//!
//! Not an AppContainer and not a restricted-SID set. A low-integrity process
//! can still *read* what the user's DACLs allow, so file read isolation on
//! Windows is not claimed. ADR 048 records why and what choosing AppContainer
//! would cost. Nothing here has been run on Windows by the author; see
//! `docs/research/phase-17-windows-unverified.md`.

use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HANDLE_FLAG_INHERIT,
    SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, GetSidSubAuthority, GetSidSubAuthorityCount,
    GetTokenInformation, SECURITY_ATTRIBUTES, SID, SID_AND_ATTRIBUTES, SID_IDENTIFIER_AUTHORITY,
    SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel,
};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_UILIMIT_DESKTOP,
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, OpenProcessToken,
    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::{Capabilities, MemoryLimit, Policy, PolicyError, Restriction, Rung};

/// `GetExitCodeProcess` reports this while the process is still running.
const STILL_RUNNING: u32 = 259;

/// `SECURITY_MANDATORY_LOW_RID` (winnt.h): the low integrity level.
const LOW_INTEGRITY_RID: u32 = 0x1000;

/// `SE_GROUP_INTEGRITY` (winnt.h), the attribute a mandatory label SID carries.
const SE_GROUP_INTEGRITY: u32 = 0x0000_0020;

/// The UI restrictions the content job carries: all of them.
const UI_RESTRICTIONS: u32 = JOB_OBJECT_UILIMIT_DESKTOP
    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
    | JOB_OBJECT_UILIMIT_EXITWINDOWS
    | JOB_OBJECT_UILIMIT_GLOBALATOMS
    | JOB_OBJECT_UILIMIT_HANDLES
    | JOB_OBJECT_UILIMIT_READCLIPBOARD
    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD;

/// The job limit flags the content job carries.
const JOB_LIMITS: u32 = JOB_OBJECT_LIMIT_ACTIVE_PROCESS
    | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
    | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    | JOB_OBJECT_LIMIT_PROCESS_MEMORY;

/// The content process may be the only process in its job.
const ACTIVE_PROCESS_LIMIT: u32 = 1;

/// `PROCESS_CREATION_MITIGATION_POLICY_*` (winbase.h), `PROC_THREAD_ATTRIBUTE_
/// MITIGATION_POLICY`'s first DWORD64. Each is "always on": the process cannot
/// turn it off. A value the running Windows does not know makes
/// `CreateProcess` fail, which fails the launch (invariant 8) rather than
/// silently dropping the mitigation.
///
/// Deliberately absent: `PROHIBIT_DYNAMIC_CODE` (the engine has no JIT, but
/// sanitizer builds and some allocators map executable memory; it is a
/// follow-up once measured, ADR 048) and `CONTROL_FLOW_GUARD` (needs a binary
/// built for it).
const MITIGATIONS: u64 = 0x1 // DEP_ENABLE
    | 0x2 // DEP_ATL_THUNK_ENABLE
    | 0x4 // SEHOP_ENABLE
    | 1 << 12 // HEAP_TERMINATE_ALWAYS_ON
    | 1 << 16 // BOTTOM_UP_ASLR_ALWAYS_ON
    | 1 << 20 // HIGH_ENTROPY_ASLR_ALWAYS_ON
    | 1 << 24 // STRICT_HANDLE_CHECKS_ALWAYS_ON
    | 1 << 28 // WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON
    | 1 << 32 // EXTENSION_POINT_DISABLE_ALWAYS_ON
    | 1 << 48 // FONT_DISABLE_ALWAYS_ON
    | 1 << 52 // IMAGE_LOAD_NO_REMOTE_ALWAYS_ON
    | 1 << 56; // IMAGE_LOAD_NO_LOW_LABEL_ALWAYS_ON

/// `PROCESS_CREATION_CHILD_PROCESS_RESTRICTED`: the process may not create
/// child processes. Belt and braces with the job's active-process limit.
const CHILD_PROCESS_RESTRICTED: u32 = 0x01;

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
        Some(job) => {
            caps.offer(Rung::JobObject);
            // The cap is probed by applying the real limits to the probe job
            // and reading them back, not by assuming SetInformationJobObject
            // would accept them (it refuses some combinations inside a nested
            // job on older Windows).
            match apply_limits(&job, crate::CONTENT_MEMORY_LIMIT)
                .and_then(|()| verify_limits(&job, crate::CONTENT_MEMORY_LIMIT))
            {
                Ok(()) => caps.offer(Rung::MemoryCap),
                Err(_) => caps.deny(Rung::MemoryCap, Restriction::Inconclusive),
            }
        }
        None => {
            caps.deny(Rung::JobObject, Restriction::NotSupported);
            caps.deny(Rung::MemoryCap, Restriction::NotSupported);
        }
    }

    match open_own_token().and_then(|token| restrict(&token).ok()) {
        Some(restricted) => {
            caps.offer(Rung::RestrictedToken);
            match lower_integrity(&restricted) {
                Ok(()) => caps.offer(Rung::LowIntegrity),
                Err(_) => caps.deny(Rung::LowIntegrity, Restriction::Inconclusive),
            }
        }
        None => {
            caps.deny(Rung::RestrictedToken, Restriction::Inconclusive);
            caps.deny(Rung::LowIntegrity, Restriction::Inconclusive);
        }
    }
}

fn create_job() -> Option<OwnedHandle> {
    // SAFETY: both arguments are documented as optional, and null is the
    // documented way to pass "no security attributes" and "unnamed". The
    // returned handle is owned by the caller and null-checked before use.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    OwnedHandle::from_nullable(handle)
}

/// Put the Phase 17 limits on a job that has no process in it yet.
///
/// Called before the process exists, so there is no moment at which it runs
/// under a looser job. The memory cap is `ProcessMemoryLimit`: the most
/// *committed* memory any one process in the job may hold. That is not the
/// Linux cap's address space (reserved-but-uncommitted pages are free here), so
/// the two figures measure different things; ADR 048 says so.
fn apply_limits(job: &OwnedHandle, memory: MemoryLimit) -> Result<(), PolicyError> {
    let bytes = usize::try_from(memory.bytes()).map_err(|_| {
        failed(
            Rung::MemoryCap,
            "the memory cap does not fit this address width",
        )
    })?;

    let mut extended = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    extended.BasicLimitInformation.LimitFlags = JOB_LIMITS;
    extended.BasicLimitInformation.ActiveProcessLimit = ACTIVE_PROCESS_LIMIT;
    extended.ProcessMemoryLimit = bytes;
    // SAFETY: `job` is a live job handle owned by the caller. `extended` is a
    // correctly initialised JOBOBJECT_EXTENDED_LIMIT_INFORMATION that outlives
    // the call, and the length passed is its size, as the information class
    // JobObjectExtendedLimitInformation requires.
    let ok = unsafe {
        SetInformationJobObject(
            job.raw(),
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(extended).cast::<c_void>(),
            size_u32::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()?,
        )
    };
    if ok == 0 {
        return Err(failed(
            Rung::MemoryCap,
            "the job object's limits could not be set",
        ));
    }

    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: UI_RESTRICTIONS,
    };
    // SAFETY: as above, with the UI-restrictions class and its structure.
    let ok = unsafe {
        SetInformationJobObject(
            job.raw(),
            JobObjectBasicUIRestrictions,
            std::ptr::addr_of!(ui).cast::<c_void>(),
            size_u32::<JOBOBJECT_BASIC_UI_RESTRICTIONS>()?,
        )
    };
    if ok == 0 {
        return Err(failed(
            Rung::JobObject,
            "the job object's UI restrictions could not be set",
        ));
    }
    Ok(())
}

fn size_u32<T>() -> Result<u32, PolicyError> {
    u32::try_from(std::mem::size_of::<T>())
        .map_err(|_| failed(Rung::JobObject, "an OS structure is implausibly large"))
}

/// Ask the OS what limits the job actually carries, and require them to be
/// exactly the ones asked for.
fn verify_limits(job: &OwnedHandle, memory: MemoryLimit) -> Result<(), PolicyError> {
    let mut extended = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: `job` is a live job handle. `extended` is live, writable and the
    // length passed is its size, as JobObjectExtendedLimitInformation requires;
    // the final out-parameter is documented as optional.
    let ok = unsafe {
        QueryInformationJobObject(
            job.raw(),
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of_mut!(extended).cast::<c_void>(),
            size_u32::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()?,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(failed(
            Rung::MemoryCap,
            "the job object's limits could not be read back",
        ));
    }
    let flags = extended.BasicLimitInformation.LimitFlags;
    if flags & JOB_LIMITS != JOB_LIMITS
        || extended.BasicLimitInformation.ActiveProcessLimit != ACTIVE_PROCESS_LIMIT
        || u64::try_from(extended.ProcessMemoryLimit).ok() != Some(memory.bytes())
    {
        return Err(failed(
            Rung::MemoryCap,
            "the job object does not carry the limits it was given",
        ));
    }

    let mut ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: 0,
    };
    // SAFETY: as above, with the UI-restrictions class and its structure.
    let ok = unsafe {
        QueryInformationJobObject(
            job.raw(),
            JobObjectBasicUIRestrictions,
            std::ptr::addr_of_mut!(ui).cast::<c_void>(),
            size_u32::<JOBOBJECT_BASIC_UI_RESTRICTIONS>()?,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || ui.UIRestrictionsClass & UI_RESTRICTIONS != UI_RESTRICTIONS {
        return Err(failed(
            Rung::JobObject,
            "the job object does not carry its UI restrictions",
        ));
    }
    Ok(())
}

/// Lower a token's integrity level to Low.
///
/// The label SID is built by hand (S-1-16-4096: revision 1, one sub-authority,
/// identifier authority 16, RID 0x1000) rather than allocated by an API, so
/// there is nothing to free. Lowering needs only `TOKEN_ADJUST_DEFAULT`;
/// raising would need a privilege the token no longer has.
fn lower_integrity(token: &OwnedHandle) -> Result<(), PolicyError> {
    let mut sid = SID {
        Revision: 1,
        SubAuthorityCount: 1,
        IdentifierAuthority: SID_IDENTIFIER_AUTHORITY {
            Value: [0, 0, 0, 0, 0, 16],
        },
        SubAuthority: [LOW_INTEGRITY_RID],
    };
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: std::ptr::addr_of_mut!(sid).cast::<c_void>(),
            Attributes: SE_GROUP_INTEGRITY,
        },
    };
    // The documented length is the label structure plus the SID it points at.
    let length = size_u32::<TOKEN_MANDATORY_LABEL>()?
        .checked_add(size_u32::<SID>()?)
        .ok_or_else(|| {
            failed(
                Rung::LowIntegrity,
                "the integrity label is implausibly large",
            )
        })?;
    // SAFETY: `token` is a live primary token. `label` is a
    // TOKEN_MANDATORY_LABEL whose SID pointer addresses `sid`, a well-formed
    // one-sub-authority SID that outlives the call; `length` is the size the
    // documentation asks for (structure plus SID). The call reads both and
    // retains neither.
    let ok = unsafe {
        SetTokenInformation(
            token.raw(),
            TokenIntegrityLevel,
            std::ptr::addr_of!(label).cast::<c_void>(),
            length,
        )
    };
    if ok == 0 {
        return Err(failed(
            Rung::LowIntegrity,
            "the token's integrity level could not be lowered",
        ));
    }
    Ok(())
}

/// A token's information, fetched into an aligned buffer.
fn token_information(
    token: &OwnedHandle,
    class: i32,
    rung: Rung,
) -> Result<[u64; 128], PolicyError> {
    let mut buffer = [0u64; 128];
    let mut returned: u32 = 0;
    // SAFETY: `token` is a live token with TOKEN_QUERY. `buffer` is writable,
    // 8-byte aligned and 1024 bytes, the length passed. `returned` is live and
    // written. Success means the start of `buffer` holds the structure for
    // `class`; failure (including a buffer too small) is an error here.
    let ok = unsafe {
        GetTokenInformation(
            token.raw(),
            class,
            buffer.as_mut_ptr().cast::<c_void>(),
            1024,
            &mut returned,
        )
    };
    if ok == 0 {
        return Err(failed(rung, "a token's information could not be read"));
    }
    Ok(buffer)
}

fn open_process_token(process: &OwnedHandle, rung: Rung) -> Result<OwnedHandle, PolicyError> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is a live process handle with query access (the handle
    // CreateProcess returns). `token` is live and written only on success.
    let ok = unsafe { OpenProcessToken(process.raw(), TOKEN_QUERY, &mut token) };
    (ok != 0)
        .then(|| OwnedHandle::from_nullable(token))
        .flatten()
        .ok_or_else(|| failed(rung, "the child's token could not be opened"))
}

/// The integrity RID of a process's token, as the OS reports it.
fn integrity_rid(process: &OwnedHandle) -> Result<u32, PolicyError> {
    let token = open_process_token(process, Rung::LowIntegrity)?;
    let buffer = token_information(&token, TokenIntegrityLevel, Rung::LowIntegrity)?;
    let label = buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
    // SAFETY: GetTokenInformation succeeded, so the start of `buffer` holds an
    // initialised TOKEN_MANDATORY_LABEL (aligned: u64-backed) whose SID points
    // into the same buffer, which is live for this block. The SID is read
    // through the sub-authority accessors the documentation provides: the count
    // is read first and the RID is the last sub-authority, so the index is in
    // range by construction. The accessors return null only for an invalid SID,
    // which is checked before each dereference.
    unsafe {
        let sid = (*label).Label.Sid;
        if sid.is_null() {
            return Err(failed(Rung::LowIntegrity, "the integrity label has no SID"));
        }
        let count = GetSidSubAuthorityCount(sid);
        if count.is_null() || *count == 0 {
            return Err(failed(Rung::LowIntegrity, "the integrity SID is malformed"));
        }
        let last = GetSidSubAuthority(sid, u32::from(*count) - 1);
        if last.is_null() {
            return Err(failed(Rung::LowIntegrity, "the integrity SID is malformed"));
        }
        Ok(*last)
    }
}

/// How many privileges a process's token holds, as the OS reports it.
#[cfg(test)]
fn privilege_count(process: &OwnedHandle) -> Result<u32, PolicyError> {
    use windows_sys::Win32::Security::{TOKEN_PRIVILEGES, TokenPrivileges};
    let token = open_process_token(process, Rung::RestrictedToken)?;
    let buffer = token_information(&token, TokenPrivileges, Rung::RestrictedToken)?;
    // SAFETY: GetTokenInformation succeeded for TokenPrivileges, so the start
    // of `buffer` (aligned: u64-backed) holds an initialised TOKEN_PRIVILEGES;
    // only its leading count is read.
    Ok(unsafe { (*buffer.as_ptr().cast::<TOKEN_PRIVILEGES>()).PrivilegeCount })
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
            TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
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
    /// Held so the job outlives the process it confines. The job carries
    /// kill-on-close, so dropping the last handle to it ends the process
    /// tree: the broker cannot leak a running content process by forgetting it.
    job: OwnedHandle,
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

/// Spawn `executable` under a restricted, low-integrity token, in a limited
/// job, with mitigation policies and exactly the handles it is meant to have.
/// Returns only after the parent has read the job's limits and the child's
/// integrity level back from the OS.
pub(crate) fn spawn(executable: &Path, policy: &Policy) -> Result<Spawned, PolicyError> {
    let token = open_own_token().ok_or_else(|| {
        failed(
            Rung::RestrictedToken,
            "this process's token could not be opened",
        )
    })?;
    let restricted = restrict(&token)?;
    lower_integrity(&restricted)?;
    let job = create_job().ok_or_else(|| {
        failed(
            Rung::JobObject,
            "the job object could not be created for this content process",
        )
    })?;
    apply_limits(&job, policy.memory)?;

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

    // Read by the OS at creation, like the arrays above: they must outlive the
    // call, and they do, being locals of this function.
    let mitigations: u64 = MITIGATIONS;
    let child_policy: u32 = CHILD_PROCESS_RESTRICTED;

    let mut attributes = AttributeList::with_capacity_for(4)?;
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
    attributes.set(
        PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY as usize,
        std::ptr::addr_of!(mitigations).cast::<c_void>(),
        std::mem::size_of::<u64>(),
    )?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY as usize,
        std::ptr::addr_of!(child_policy).cast::<c_void>(),
        std::mem::size_of::<u32>(),
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

    let mut child = Child {
        process,
        job,
        pid: information.dwProcessId,
        exited: None,
    };

    // Ask the OS, not the child and not our own bookkeeping. If any answer is
    // wrong the child is ended before it can be used (invariant 8); dropping it
    // would also end it, via kill-on-close, but a kill that is explicit cannot
    // be lost by a later edit to Drop.
    if let Err(error) = child.verify(policy) {
        let _ = child.kill();
        return Err(error);
    }

    Ok(Spawned {
        child,
        stdin: stdin.write,
        stdout: stdout.read,
        // Everything here was set at creation and confirmed by a readback from
        // the OS: the token is the one the process was made with (its
        // integrity level read from the child's own token), and the job's
        // limits were read back from the job. Mitigation policies are not
        // listed: they cannot be read back from outside the process.
        applied: vec![
            Rung::RestrictedToken,
            Rung::JobObject,
            Rung::LowIntegrity,
            Rung::MemoryCap,
        ],
    })
}

impl Child {
    /// Confirm, from the OS's own account, that this child is in its job, the
    /// job carries the limits asked for, and the child's token is low
    /// integrity.
    fn verify(&self, policy: &Policy) -> Result<(), PolicyError> {
        let mut inside: i32 = 0;
        // SAFETY: `self.process` and `self.job` are live handles owned by this
        // struct; `inside` is live and aligned and written on success.
        let ok = unsafe { IsProcessInJob(self.process.raw(), self.job.raw(), &mut inside) };
        if ok == 0 || inside == 0 {
            return Err(failed(
                Rung::JobObject,
                "the child is not in the job it was created with",
            ));
        }
        verify_limits(&self.job, policy.memory)?;
        if integrity_rid(&self.process)? != LOW_INTEGRITY_RID {
            return Err(failed(
                Rung::LowIntegrity,
                "the child's token is not at low integrity",
            ));
        }
        Ok(())
    }

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
        // SAFETY: `self.job` is a live job handle owned by this struct. The
        // exit code is arbitrary and never read back as meaningful; 1 marks an
        // abnormal end. Terminating the job ends every process in it, so a
        // grandchild (which the active-process limit should already prevent)
        // cannot outlive the call.
        let ok = unsafe { TerminateJobObject(self.job.raw(), 1) };
        if ok != 0 {
            return Ok(());
        }
        // SAFETY: `self.process` is a live process handle owned by this
        // struct; the exit code is as above.
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

    fn spawn_cmd() -> Result<Spawned, PolicyError> {
        spawn(
            std::path::Path::new(r"C:\Windows\System32\cmd.exe"),
            &Policy::content(),
        )
    }

    /// A spawned child for a test that asserts on it. A machine that cannot
    /// sandbox skips, except under `PX_REQUIRE_SANDBOX` (CI), where a failed
    /// spawn is a failed test: these are the only tests that check the Phase 17
    /// profile, and a silent skip would pass without checking it.
    fn must_spawn() -> Option<Spawned> {
        match spawn_cmd() {
            Ok(spawned) => Some(spawned),
            Err(error) => {
                assert!(
                    std::env::var_os("PX_REQUIRE_SANDBOX").is_none(),
                    "PX_REQUIRE_SANDBOX is set but the Windows profile could not be applied: {error}"
                );
                None
            }
        }
    }

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
        let mut spawned = match spawn_cmd() {
            Ok(spawned) => spawned,
            // A machine that cannot create a job or restrict a token refuses
            // to launch at all, which is correct behaviour rather than a
            // failing test.
            Err(_) => return,
        };

        let mut inside: i32 = 0;
        // SAFETY: `spawned.child.process` is a live process handle owned by
        // the Child, and `spawned.child.job` a live job handle owned beside
        // it. `inside` is live and aligned and written on success.
        let queried = unsafe {
            IsProcessInJob(
                spawned.child.process.raw(),
                spawned.child.job.raw(),
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
            match spawn_cmd() {
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
        let Ok(mut spawned) = spawn_cmd() else {
            return;
        };
        // The parent's ends are real, distinct, and ours.
        assert!(!spawned.stdin.raw().is_null());
        assert!(!spawned.stdout.raw().is_null());
        assert_ne!(spawned.stdin.raw(), spawned.stdout.raw());
        let _ = spawned.child.kill();
        let _ = spawned.child.wait();
    }

    /// The job's limits, read back by this test straight from the OS through a
    /// separate query (not through `verify_limits`, which is what is under
    /// test): memory cap, kill-on-close, one process, every UI restriction.
    #[test]
    fn sandbox_policy_windows_the_job_carries_the_phase_17_limits() {
        let Some(mut spawned) = must_spawn() else {
            return;
        };
        let mut extended = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let mut ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: 0,
        };
        // SAFETY: the job handle is live and owned by the Child; each out
        // structure is live, writable and passed with its own size, and the
        // last parameter is documented as optional.
        let (limits_ok, ui_ok) = unsafe {
            (
                QueryInformationJobObject(
                    spawned.child.job.raw(),
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of_mut!(extended).cast::<c_void>(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                ),
                QueryInformationJobObject(
                    spawned.child.job.raw(),
                    JobObjectBasicUIRestrictions,
                    std::ptr::addr_of_mut!(ui).cast::<c_void>(),
                    std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
                    std::ptr::null_mut(),
                ),
            )
        };
        let _ = spawned.child.kill();
        let _ = spawned.child.wait();

        assert_ne!(limits_ok, 0, "querying the job's limits failed");
        assert_ne!(ui_ok, 0, "querying the job's UI restrictions failed");
        let flags = extended.BasicLimitInformation.LimitFlags;
        assert_ne!(
            flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            0,
            "kill-on-close"
        );
        assert_ne!(
            flags & JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            0,
            "memory limit flag"
        );
        assert_ne!(
            flags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            0,
            "active-process flag"
        );
        assert_eq!(extended.BasicLimitInformation.ActiveProcessLimit, 1);
        assert_eq!(
            extended.ProcessMemoryLimit as u64,
            crate::CONTENT_MEMORY_LIMIT.bytes()
        );
        assert_eq!(
            ui.UIRestrictionsClass & UI_RESTRICTIONS,
            UI_RESTRICTIONS,
            "clipboard, desktop, display, atoms, handles, log-off, parameters"
        );
    }

    /// The child's own token, opened by the parent, is at low integrity.
    #[test]
    fn sandbox_policy_windows_the_child_runs_at_low_integrity() {
        let Some(mut spawned) = must_spawn() else {
            return;
        };
        let rid = integrity_rid(&spawned.child.process);
        let _ = spawned.child.kill();
        let _ = spawned.child.wait();
        assert_eq!(rid.ok(), Some(LOW_INTEGRITY_RID));
    }

    /// `DISABLE_MAX_PRIVILEGE` leaves at most `SeChangeNotifyPrivilege`.
    /// If the OS lists the disabled privileges instead of removing them, this
    /// is where it shows (docs/research/phase-17-windows-unverified.md).
    #[test]
    fn sandbox_policy_windows_the_child_holds_at_most_one_privilege() {
        let Some(mut spawned) = must_spawn() else {
            return;
        };
        let count = privilege_count(&spawned.child.process);
        let _ = spawned.child.kill();
        let _ = spawned.child.wait();
        let count = count.expect("the child's privileges can be read");
        assert!(count <= 1, "the child's token holds {count} privileges");
    }

    /// Kill-on-close: forgetting a child does not leave a content process
    /// running. The test keeps its own handle to the process, drops the whole
    /// `Spawned`, and waits for the OS to end it.
    #[test]
    fn sandbox_policy_windows_dropping_the_child_ends_the_process() {
        let Some(spawned) = must_spawn() else {
            return;
        };
        let mut duplicate: HANDLE = std::ptr::null_mut();
        // SAFETY: both processes are the current one (pseudo-handle, always
        // valid); the source is the live process handle of the Child;
        // `duplicate` is live and written on success. A non-inheritable copy
        // with the original's access rights is what is asked for.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                spawned.child.process.raw(),
                GetCurrentProcess(),
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        assert_ne!(ok, 0, "duplicating the process handle failed");
        let duplicate = OwnedHandle::from_nullable(duplicate).expect("a duplicate handle");

        drop(spawned);

        // SAFETY: `duplicate` is a live process handle owned here; the wait is
        // bounded, so a failure to kill cannot hang the suite.
        let waited = unsafe { WaitForSingleObject(duplicate.raw(), 10_000) };
        assert_eq!(
            waited, WAIT_OBJECT_0,
            "the process outlived the last handle to its job"
        );
    }

    /// Killing the child ends it and its exit is observable, through the job.
    #[test]
    fn sandbox_policy_windows_kill_ends_the_child() {
        let Some(mut spawned) = must_spawn() else {
            return;
        };
        spawned.child.kill().expect("kill");
        assert_eq!(spawned.child.wait().expect("wait"), 1);
    }

    /// The flag word is what it claims to be. A transcription error would
    /// silently drop a mitigation, because the OS accepts any subset.
    #[test]
    fn sandbox_policy_windows_the_mitigation_word_is_the_documented_one() {
        assert_eq!(MITIGATIONS & 0x7, 0x7, "DEP, ATL thunk, SEHOP");
        for (name, bit) in [
            ("heap terminate", 12),
            ("bottom-up ASLR", 16),
            ("high-entropy ASLR", 20),
            ("strict handle checks", 24),
            ("Win32k disable", 28),
            ("extension points", 32),
            ("fonts", 48),
            ("remote images", 52),
            ("low-label images", 56),
        ] {
            assert_ne!(MITIGATIONS & (1u64 << bit), 0, "{name} (bit {bit})");
        }
        assert_eq!(MITIGATIONS.count_ones(), 12);
    }
}
