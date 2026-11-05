# Phase 17, Windows: what is verified and what is not

ADR 044 describes the profile. It was written on Linux. What was and was not
checked:

## Checked (Linux container)

- `cargo check` and `cargo clippy -D warnings --all-targets` for
  `x86_64-pc-windows-gnu`: the code, including the Windows-only tests,
  type-checks against `windows-sys` 0.61.2. Nothing was linked or run.
- The Linux build, tests and gates are unchanged.

## Not checked: must be seen green or red in Windows CI

Run `cargo test -p px-sandbox` with `PX_REQUIRE_SANDBOX=1` on Windows. The
tests that check the profile (`sandbox_policy_windows_*`,
`sandbox_memory_cap_windows_*`) panic rather than skip if the profile cannot be
applied. Each item below is a guess to confirm; the first failure will almost
certainly be one of them.

1. **`CreateRestrictedToken` result has `TOKEN_ADJUST_DEFAULT`.** The Low label
   is set on the restricted token with `SetTokenInformation`. The source token is
   opened with `TOKEN_ADJUST_DEFAULT` added, but the documentation does not say
   the derived handle inherits that right. If `lower_integrity` fails with
   access denied, duplicate with `DuplicateTokenEx` and an explicit mask.
2. **`DISABLE_MAX_PRIVILEGE` removes rather than disables.** The test
   `..._holds_at_most_one_privilege` reads the child's privilege count. If the
   token lists them disabled, a process can re-enable them; the fix is
   `PrivilegesToDelete`. The spawn path does not itself verify this.
3. **A Low-integrity, restricted-token child can start.** It must read its
   executable and the loader's DLLs and use the inherited pipes. Low integrity
   restricts writes, not reads, so this is expected; `cmd.exe` is the stand-in.
4. **`PROC_THREAD_ATTRIBUTE_JOB_LIST` with `ACTIVE_PROCESS` = 1.** The child is
   the job's one process. Expected fine; a job that already counts a second
   process (a runner that nests jobs) is not.
5. **Every mitigation bit is honoured on the CI image**, notably
   `WIN32K_SYSTEM_CALL_DISABLE`, fonts disabled and `IMAGE_LOAD_NO_LOW_LABEL`.
   The constants are typed by hand from `winbase.h` (a unit test pins the bit
   positions, not their meaning). An unknown bit fails `CreateProcess`, so a
   wrong constant shows as a spawn failure, not as silence. A child that starts
   and then dies in the loader because of Win32k lockdown would show as the
   readback tests failing (`OpenProcessToken` on an exited process) or as
   `px-content` failing to answer.
6. **Child-process policy attribute** needs Windows 10 1709+; older fails closed.
7. **Mitigation policies are not read back** (cannot be from outside), so no
   test proves they are in force. A process-internal probe calling
   `GetProcessMitigationPolicy` would, under the same "the child's report is
   not evidence" caveat as the Linux `verify_stage_two`.
8. **The commit limit's figure.** 1 GiB commit was not measured on Windows for
   the content or GPU process.
9. **`JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`** is untested against the
   crash tests; a crash should still be a crash to the broker.

## Not built

- AppContainer / restricted SIDs (file read, network), decision 1 in ADR 044.
- Windows stage two (`enter_content_profile` still refuses) and the Windows
  escape suite (`sandbox_escape_*`, probe bodies) and runaway-tab test
  (`sandbox_memory_*`): Linux-only today.
- Prohibit-dynamic-code and control-flow-guard mitigations.
- GPU profile (`verify_gpu_profile` still refuses on Windows).
