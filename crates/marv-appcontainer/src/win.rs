//! The Win32 calls: the container's SID, the rules on its paths, and the process started inside it.

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, GENERIC_ALL, HANDLE, LocalFree, WAIT_FAILED};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE,
    REVOKE_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, FreeSid, NO_INHERITANCE, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
    SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread, STARTUPINFOEXW, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

use crate::spec::{Access, Grant, Spec, command_line, container_name, env_block, read_record, to_write};

/// `SE_GROUP_ENABLED`: a capability the process holds.
const SE_GROUP_ENABLED: u32 = 4;
/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`: the container was made by an earlier run.
const ALREADY_EXISTS: i32 = 0x800700B7_u32 as i32;
/// `internetClient` and `privateNetworkClientServer`: the network, out and on the local network.
const NETWORK_CAPABILITIES: [&str; 2] = ["S-1-15-3-1", "S-1-15-3-3"];

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// A SID Windows allocated, freed with the function that matches how it was made.
struct Sid {
    ptr: PSID,
    local: bool,
}

impl Drop for Sid {
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }
        // SAFETY: `ptr` came from the API named by `local` (ConvertStringSidToSidW: LocalFree; the AppContainer APIs:
        // FreeSid), is freed once (here), and nothing uses it after the drop.
        unsafe {
            if self.local {
                LocalFree(self.ptr);
            } else {
                FreeSid(self.ptr);
            }
        }
    }
}

/// An open handle, closed when dropped.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle was returned open by Windows to this struct alone and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// The container `name`'s SID: made the first time, derived from the name after that.
fn container_sid(name: &str) -> Result<Sid, String> {
    let wname = wide(name.as_ref());
    let display = wide(format!("Marv: {name}").as_ref());
    let mut sid: PSID = null_mut();
    // SAFETY: every string is NUL-terminated UTF-16 that outlives the call; no capabilities are passed (null, 0);
    // `sid` is a valid out pointer that receives a SID we free with FreeSid.
    let made = unsafe { CreateAppContainerProfile(wname.as_ptr(), display.as_ptr(), display.as_ptr(), null(), 0, &mut sid) };
    if made >= 0 {
        return Ok(Sid { ptr: sid, local: false });
    }
    if made != ALREADY_EXISTS {
        return Err(format!("Couldn't make the sandbox's AppContainer (error {made:#x})."));
    }
    // SAFETY: as above: a NUL-terminated name and a valid out pointer, freed with FreeSid.
    let derived = unsafe { DeriveAppContainerSidFromAppContainerName(wname.as_ptr(), &mut sid) };
    if derived < 0 {
        return Err(format!("Couldn't find the sandbox's AppContainer (error {derived:#x})."));
    }
    Ok(Sid { ptr: sid, local: false })
}

fn capability_sids(network: bool) -> Result<Vec<Sid>, String> {
    let names: &[&str] = if network { &NETWORK_CAPABILITIES } else { &[] };
    names
        .iter()
        .map(|name| {
            let text = wide(name.as_ref());
            let mut sid: PSID = null_mut();
            // SAFETY: a NUL-terminated string SID and a valid out pointer; the SID is freed with LocalFree.
            let ok = unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) };
            if ok == 0 { Err(format!("Couldn't read the capability {name}.")) } else { Ok(Sid { ptr: sid, local: true }) }
        })
        .collect()
}

/// The access bits a rule names. A denial of writing leaves out `SYNCHRONIZE` and `READ_CONTROL` (both in
/// `FILE_GENERIC_WRITE`), which reading needs.
fn mask(access: Access) -> u32 {
    match access {
        Access::Read => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        Access::ReadWrite => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE | FILE_GENERIC_WRITE | DELETE | FILE_DELETE_CHILD,
        Access::DenyWrite => {
            FILE_WRITE_DATA
                | FILE_APPEND_DATA
                | FILE_WRITE_EA
                | FILE_WRITE_ATTRIBUTES
                | DELETE
                | FILE_DELETE_CHILD
                | WRITE_DAC
                | WRITE_OWNER
        }
        Access::DenyAll => GENERIC_ALL,
    }
}

/// Adds (`Some`) or removes (`None`: every rule for this SID) the container's rule on `path`.
fn set_rule(path: &Path, sid: &Sid, access: Option<Access>) -> Result<(), String> {
    let wpath = wide(path.as_os_str());
    let (mode, bits) = match access {
        Some(a) if a.is_deny() => (DENY_ACCESS, mask(a)),
        Some(a) => (GRANT_ACCESS, mask(a)),
        None => (REVOKE_ACCESS, 0),
    };
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: bits,
        grfAccessMode: mode,
        grfInheritance: if path.is_dir() { SUB_CONTAINERS_AND_OBJECTS_INHERIT } else { NO_INHERITANCE },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.ptr.cast(),
        },
    };
    let mut old: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: a NUL-terminated path; only the DACL is asked for, so the owner, group and SACL out pointers may be null;
    // `old` points into `descriptor`, which we free with LocalFree after its last use below.
    let got = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut old,
            null_mut(),
            &mut descriptor,
        )
    };
    if got != ERROR_SUCCESS {
        return Err(format!("Couldn't read the permissions of {} (error {got}).", path.display()));
    }
    let mut new: *mut ACL = null_mut();
    // SAFETY: one valid entry whose trustee SID outlives the call; `old` is the DACL just read (still owned by
    // `descriptor`); `new` receives a fresh ACL we free with LocalFree.
    let merged = unsafe { SetEntriesInAclW(1, &entry, old, &mut new) };
    let result = if merged != ERROR_SUCCESS {
        Err(format!("Couldn't add the sandbox's rule for {} (error {merged}).", path.display()))
    } else {
        // SAFETY: a NUL-terminated path and the ACL just built; only the DACL is set, so the other pointers are null.
        let set = unsafe {
            SetNamedSecurityInfoW(wpath.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, null_mut(), null_mut(), new, null())
        };
        if set == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("Couldn't set the sandbox's rule on {} (error {set}).", path.display()))
        }
    };
    // SAFETY: both were allocated by the calls above for us to free with LocalFree, and aren't used after this.
    unsafe {
        if !new.is_null() {
            LocalFree(new.cast());
        }
        LocalFree(descriptor);
    }
    result
}

/// A job that ends the command and all it starts when the launcher goes, however it goes (a stop's `taskkill` of the
/// launcher alone, a crash).
fn kill_on_close_job() -> Result<Handle, String> {
    // SAFETY: default security and no name; the handle is owned by the returned `Handle`.
    let job = Handle(unsafe { CreateJobObjectW(null(), null()) });
    if job.0.is_null() {
        return Err("Couldn't make the sandbox's job.".into());
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = Default::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `limits` is the struct this information class expects, passed with its exact size.
    let ok = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 { Err("Couldn't set up the sandbox's job.".into()) } else { Ok(job) }
}

/// Starts `command` in the container (suspended, put in `job`, then resumed) and waits for its exit code. It inherits
/// this process's console and standard handles, so in an agent's terminal it draws there.
fn spawn_and_wait(spec: &Spec, sid: &Sid, caps: &[Sid], command: &[OsString], job: &Handle) -> Result<u32, String> {
    let mut attrs: Vec<SID_AND_ATTRIBUTES> =
        caps.iter().map(|c| SID_AND_ATTRIBUTES { Sid: c.ptr, Attributes: SE_GROUP_ENABLED }).collect();
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: sid.ptr,
        Capabilities: if attrs.is_empty() { null_mut() } else { attrs.as_mut_ptr() },
        CapabilityCount: attrs.len() as u32,
        Reserved: 0,
    };
    let mut size = 0usize;
    // SAFETY: the documented size query: a null list with one attribute writes the size needed into `size` (and fails
    // with ERROR_INSUFFICIENT_BUFFER, which is expected).
    unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
    let mut buffer = vec![0u8; size.max(1)];
    let list: LPPROC_THREAD_ATTRIBUTE_LIST = buffer.as_mut_ptr().cast();
    // SAFETY: `buffer` has the size just asked for and lives until DeleteProcThreadAttributeList below.
    if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
        return Err("Couldn't prepare the sandbox's process attributes.".into());
    }
    let started = (|| {
        // SAFETY: `capabilities` (and the SIDs and `attrs` it points to) outlive CreateProcessW, which reads the
        // attribute; the size is the struct's own.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                (&capabilities as *const SECURITY_CAPABILITIES).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                null_mut(),
                null(),
            )
        };
        if updated == 0 {
            return Err("Couldn't give the command its AppContainer.".to_string());
        }
        let mut info: STARTUPINFOEXW = Default::default();
        info.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        info.lpAttributeList = list;
        let mut line: Vec<u16> = command_line(command).encode_utf16().chain(std::iter::once(0)).collect();
        let env = env_block(&spec.env);
        let cwd = wide(spec.cwd.as_os_str());
        let mut process: PROCESS_INFORMATION = Default::default();
        // SAFETY: `line` is a writable NUL-terminated buffer (CreateProcessW may change it); `env` is a complete
        // UTF-16 block (CREATE_UNICODE_ENVIRONMENT); `cwd` is NUL-terminated; `info` is an initialised STARTUPINFOEXW
        // whose attribute list stays alive; handles are inherited so the console and stdio carry over.
        let ok = unsafe {
            CreateProcessW(
                null(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
                env.as_ptr().cast(),
                cwd.as_ptr(),
                &info.StartupInfo,
                &mut process,
            )
        };
        if ok == 0 {
            return Err(format!(
                "Couldn't start {} in the sandbox: {}",
                command[0].to_string_lossy(),
                std::io::Error::last_os_error()
            ));
        }
        Ok((Handle(process.hProcess), Handle(process.hThread)))
    })();
    // SAFETY: the list was initialised above and is deleted once, after CreateProcessW has used it.
    unsafe { DeleteProcThreadAttributeList(list) };
    let (process, thread) = started?;
    // SAFETY: both handles are open and ours; a failed assignment is reported and the process ended with the job.
    if unsafe { AssignProcessToJobObject(job.0, process.0) } == 0 {
        return Err("Couldn't put the sandboxed command in its job.".into());
    }
    // SAFETY: the thread handle is the suspended main thread just made.
    unsafe { ResumeThread(thread.0) };
    drop(thread);
    // SAFETY: an open process handle; INFINITE waits until it ends.
    if unsafe { WaitForSingleObject(process.0, INFINITE) } == WAIT_FAILED {
        return Err("Lost track of the sandboxed command.".into());
    }
    let mut code = 1u32;
    // SAFETY: an open, ended process handle and a valid out pointer.
    unsafe { GetExitCodeProcess(process.0, &mut code) };
    Ok(code)
}

pub fn run(spec: &Spec, command: &[OsString]) -> Result<u32, String> {
    if command.is_empty() {
        return Err("Nothing to run.".into());
    }
    let sid = container_sid(&container_name(&spec.profile))?;
    let caps = capability_sids(spec.network)?;
    let mut recorded = read_record(spec.record.as_ref().and_then(|r| std::fs::read_to_string(r).ok()).as_deref());
    let mut one_run: Vec<&Grant> = Vec::new();
    let result = (|| {
        for grant in to_write(spec, &recorded) {
            if !grant.path.exists() {
                continue;
            }
            set_rule(&grant.path, &sid, Some(grant.access))?;
            if grant.lasting {
                // A newer rule on the same path replaces the one recorded (read-only checks, then read-write).
                recorded.retain(|r| r.path != grant.path);
                recorded.push(grant.clone());
            } else {
                one_run.push(grant);
            }
        }
        if let Some(record) = &spec.record {
            if let Some(dir) = record.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(record, serde_json::to_string(&recorded).unwrap_or_default());
        }
        let job = kill_on_close_job()?;
        spawn_and_wait(spec, &sid, &caps, command, &job)
    })();
    // This run's own rules go again. Removing takes every rule this container has on the path, so a lasting rule on
    // the same path is written back.
    for grant in one_run.iter().rev() {
        let _ = set_rule(&grant.path, &sid, None);
        if let Some(lasting) = recorded.iter().find(|r| r.path == grant.path) {
            let _ = set_rule(&lasting.path, &sid, Some(lasting.access));
        }
    }
    result
}
