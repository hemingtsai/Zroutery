use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::path::Path;
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::*,
    Security::Isolation::*,
    Security::*,
    Storage::FileSystem::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ},
    System::{JobObjects::*, Pipes::*, Threading::*},
};
use zroutery_plugin_api::{PluginError, Result};

struct Handle(HANDLE);
impl Handle {
    fn terminate_job(self) {
        unsafe {
            TerminateJobObject(self.0, 78);
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
unsafe impl Send for Handle {}

pub struct RestrictedProcess {
    process: Handle,
    job: Handle,
    pub input: File,
    pub output: File,
    /// The per-launch AppContainer profile this worker runs under. The launch
    /// creates it fresh and never reuses it, so the caller can delete it once
    /// the process is gone instead of letting profiles accumulate per spawn.
    pub profile_name: String,
}
impl RestrictedProcess {
    pub fn wait(&self, milliseconds: u32) -> Result<u32> {
        let wait = unsafe { WaitForSingleObject(self.process.0, milliseconds) };
        if wait == WAIT_TIMEOUT {
            self.terminate();
            return Err(PluginError::PluginTimeout);
        }
        if wait != WAIT_OBJECT_0 {
            self.terminate();
            return Err(PluginError::PluginCrashed);
        }
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.process.0, &mut code) } == 0 {
            return Err(PluginError::PluginCrashed);
        }
        Ok(code)
    }
    pub fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.job.0, 78);
        }
    }
    pub fn watchdog(&self) -> Result<impl FnOnce() + Send + 'static> {
        let mut duplicate = std::ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.job.0,
                GetCurrentProcess(),
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(PluginError::SandboxUnavailable);
        }
        let job = Handle(duplicate);
        Ok(move || job.terminate_job())
    }
}
impl Drop for RestrictedProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

/// Best-effort deletion of one per-launch AppContainer profile. The profile is
/// created fresh per spawn and is never reused, so deleting it as soon as the
/// worker is gone keeps the profile store from accumulating one entry per
/// render. Returns whether the OS accepted the deletion; a failure leaves the
/// name for the caller's ledger and the next sweep.
pub fn delete_app_container_profile(name: &str) -> bool {
    let name = wide(name);
    let deleted = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    deleted == 0
}
fn unavailable(stage: &'static str) -> PluginError {
    // Host-generated diagnostic only; no package/IPC/error body is logged.
    #[cfg(debug_assertions)]
    eprintln!("sandbox-launch: stage={stage} os_code={}", unsafe {
        GetLastError()
    });
    #[cfg(not(debug_assertions))]
    let _ = stage;
    PluginError::SandboxUnavailable
}
fn allow_worker_read(path: &Path, sid: PSID) -> Result<()> {
    let path = wide(&path.display().to_string());
    let mut old_acl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_acl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    } != 0
    {
        return Err(unavailable("read_worker_acl"));
    }
    let mut access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 0,
        ..Default::default()
    };
    access.Trustee.TrusteeForm = TRUSTEE_IS_SID;
    access.Trustee.TrusteeType = TRUSTEE_IS_UNKNOWN;
    access.Trustee.ptstrName = sid.cast();
    let mut acl = std::ptr::null_mut();
    let status = unsafe { SetEntriesInAclW(1, &access, old_acl, &mut acl) };
    if status != 0 {
        unsafe {
            LocalFree(descriptor);
        }
        return Err(unavailable("build_worker_acl"));
    }
    let status = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    unsafe {
        LocalFree(acl.cast());
        LocalFree(descriptor);
    }
    if status != 0 {
        return Err(unavailable("write_worker_acl"));
    }
    Ok(())
}

/// Run an isolated OS negative fixture: file/network/parent/child denied.
/// Uses only synthetic canaries under `temp_root`; never user config/credentials.
pub fn probe(worker: &Path, temp_root: &Path) -> Result<crate::supervisor::SandboxStatus> {
    std::fs::create_dir_all(temp_root).map_err(|_| PluginError::SandboxUnavailable)?;
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let probe_root = temp_root.join(format!("sandbox-probe-{nonce}"));
    std::fs::create_dir_all(&probe_root).map_err(|_| PluginError::SandboxUnavailable)?;
    let secret = probe_root.join("host-canary.txt");
    std::fs::write(&secret, "synthetic-host-only-canary")
        .map_err(|_| PluginError::SandboxUnavailable)?;
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|_| PluginError::SandboxUnavailable)?;
    let port = listener
        .local_addr()
        .map_err(|_| PluginError::SandboxUnavailable)?
        .port();
    let work = probe_root.join("worker-root");
    std::fs::create_dir_all(&work).map_err(|_| PluginError::SandboxUnavailable)?;
    let arguments = format!(
        "--sandbox-probe {} {} {}",
        secret.display(),
        std::process::id(),
        port
    );
    let result = (|| {
        let process = launch(worker, &work, &arguments)?;
        let profile = process.profile_name.clone();
        let outcome = process.wait(5000);
        // The probe worker is done either way; its profile must not linger.
        let _ = delete_app_container_profile(&profile);
        match outcome {
            Ok(0) => Ok(crate::supervisor::SandboxStatus::Verified),
            _ => Ok(crate::supervisor::SandboxStatus::Unavailable),
        }
    })();
    let _ = listener;
    let _ = std::fs::remove_dir_all(&probe_root);
    result
}

/// Explicit worker pathname, clean environment, private pipes, AppContainer
/// with ZERO capabilities, no children, restricted UI and 64 MiB job ceiling.
/// Caller supplies a trusted executable and an isolated empty working root.
pub fn launch(
    executable: &Path,
    working_root: &Path,
    arguments: &str,
) -> Result<RestrictedProcess> {
    let executable = executable
        .canonicalize()
        .map_err(|_| PluginError::SandboxUnavailable)?;
    let working_root = working_root
        .canonicalize()
        .map_err(|_| PluginError::SandboxUnavailable)?;
    if !executable.is_file()
        || !working_root.is_dir()
        || arguments.contains(['"', '\0', '\r', '\n'])
    {
        return Err(PluginError::SandboxUnavailable);
    }
    // Per-process profile: no plugin/facet/account shares writable AppContainer
    // profile state or the ACL of another worker's helper copy.
    let profile_name = format!("org.zroutery.worker.{}", uuid::Uuid::new_v4().simple());
    let name = wide(&profile_name);
    let mut sid = std::ptr::null_mut();
    // A derived SID without a registered profile can make LowBox process
    // creation fail with ERROR_FILE_NOT_FOUND. Registration has no capabilities.
    let created = unsafe {
        CreateAppContainerProfile(
            name.as_ptr(),
            name.as_ptr(),
            name.as_ptr(),
            std::ptr::null(),
            0,
            &mut sid,
        )
    };
    if created < 0
        && unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) } < 0
    {
        return Err(PluginError::SandboxUnavailable);
    }
    struct Sid(PSID);
    impl Drop for Sid {
        fn drop(&mut self) {
            unsafe {
                FreeSid(self.0);
            }
        }
    }
    let sid = Sid(sid);
    // Grant RX only to an isolated copy of the trusted helper and its empty
    // working root. Never widen a project/user directory or host configuration.
    let worker_copy = working_root.join(format!("worker-{}.exe", uuid::Uuid::new_v4().simple()));
    if !worker_copy.exists() {
        let bytes = std::fs::read(&executable).map_err(|_| PluginError::SandboxUnavailable)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&worker_copy)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(&bytes)?;
                file.sync_all()
            })
            .map_err(|_| PluginError::SandboxUnavailable)?;
    }
    allow_worker_read(&working_root, sid.0)?;
    allow_worker_read(&worker_copy, sid.0)?;
    let executable = worker_copy;
    // Only Windows-owned runtime dependencies are copied into this private root
    // when the worker/QuickJS object imports them. Never search PATH or arbitrary
    // DLL locations. System DLLs retain normal OS loading/verification.
    let system_root = std::env::var("SystemRoot").map_err(|_| PluginError::SandboxUnavailable)?;
    for runtime in [
        "ucrtbase.dll",
        "api-ms-win-crt-string-l1-1-0.dll",
        "api-ms-win-crt-heap-l1-1-0.dll",
        "api-ms-win-crt-runtime-l1-1-0.dll",
        "api-ms-win-crt-convert-l1-1-0.dll",
        "api-ms-win-crt-stdio-l1-1-0.dll",
        "api-ms-win-crt-math-l1-1-0.dll",
        "api-ms-win-crt-utility-l1-1-0.dll",
        "api-ms-win-crt-locale-l1-1-0.dll",
        "api-ms-win-core-synch-l1-2-0.dll",
    ] {
        for base in [
            Path::new(&system_root).join("System32"),
            Path::new(&system_root).join("System32").join("downlevel"),
        ] {
            let source = base.join(runtime);
            let destination = working_root.join(runtime);
            if source.is_file() && !destination.exists() {
                std::fs::copy(&source, &destination)
                    .map_err(|_| PluginError::SandboxUnavailable)?;
                allow_worker_read(&destination, sid.0)?;
                break;
            }
        }
    }
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: sid.0,
        Capabilities: std::ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let mut attributes_size = 0usize;
    unsafe {
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 3, 0, &mut attributes_size);
    }
    let mut attributes = vec![0usize; attributes_size.div_ceil(std::mem::size_of::<usize>())];
    let list = attributes.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
    if unsafe { InitializeProcThreadAttributeList(list, 3, 0, &mut attributes_size) } == 0 {
        return Err(PluginError::SandboxUnavailable);
    }
    struct Attributes(LPPROC_THREAD_ATTRIBUTE_LIST);
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe {
                DeleteProcThreadAttributeList(self.0);
            }
        }
    }
    let _attributes = Attributes(list);
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let (mut stdin_read, mut stdin_write, mut stdout_read, mut stdout_write) = (
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    if unsafe { CreatePipe(&mut stdin_read, &mut stdin_write, &security, 64 * 1024) } == 0 {
        return Err(PluginError::SandboxUnavailable);
    }
    let mut stdin_read = Handle(stdin_read);
    let mut stdin_write = Handle(stdin_write);
    if unsafe { CreatePipe(&mut stdout_read, &mut stdout_write, &security, 64 * 1024) } == 0 {
        return Err(PluginError::SandboxUnavailable);
    }
    let mut stdout_read = Handle(stdout_read);
    let stdout_write = Handle(stdout_write);
    if unsafe { SetHandleInformation(stdin_write.0, HANDLE_FLAG_INHERIT, 0) } == 0
        || unsafe { SetHandleInformation(stdout_read.0, HANDLE_FLAG_INHERIT, 0) } == 0
    {
        return Err(PluginError::SandboxUnavailable);
    }
    let inherited = [stdin_read.0, stdout_write.0];
    let child_policy = 1u32; // PROCESS_CREATION_CHILD_PROCESS_RESTRICTED
    for (attribute, pointer, size) in [
        (
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            (&capabilities as *const SECURITY_CAPABILITIES).cast::<c_void>(),
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
        ),
        (
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            inherited.as_ptr().cast::<c_void>(),
            std::mem::size_of_val(&inherited),
        ),
        (
            PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
            (&child_policy as *const u32).cast::<c_void>(),
            std::mem::size_of::<u32>(),
        ),
    ] {
        if unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                attribute as usize,
                pointer,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(PluginError::SandboxUnavailable);
        }
    }
    let job = Handle(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) });
    if job.0.is_null() {
        return Err(PluginError::SandboxUnavailable);
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_JOB_MEMORY
        | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit = 64 * 1024 * 1024;
    limits.JobMemoryLimit = 64 * 1024 * 1024;
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(PluginError::SandboxUnavailable);
    }
    // Desktop/handles clipboard/global atom limits are enough to contain UI;
    // JOB_OBJECT_UILIMIT_HANDLES can interfere with early process initialization
    // in a no-capability AppContainer (e.g. loader/console handle setup).
    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT_READCLIPBOARD
            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
            | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
            | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
            | JOB_OBJECT_UILIMIT_EXITWINDOWS
            | JOB_OBJECT_UILIMIT_DESKTOP
            | JOB_OBJECT_UILIMIT_GLOBALATOMS,
    };
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectBasicUIRestrictions,
            (&ui as *const JOBOBJECT_BASIC_UI_RESTRICTIONS).cast(),
            std::mem::size_of_val(&ui) as u32,
        )
    } == 0
    {
        return Err(PluginError::SandboxUnavailable);
    }
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin_read.0;
    startup.StartupInfo.hStdOutput = stdout_write.0;
    startup.StartupInfo.hStdError = stdout_write.0; // trusted worker never logs raw JS
    startup.lpAttributeList = list;
    // CreateProcess command parsing does not accept every extended-path spelling
    // canonicalize returns (notably when the token is LowBox). Use the normal
    // absolute form, still never PATH/shell lookup.
    let executable_text = executable.display().to_string();
    let executable_text = executable_text
        .strip_prefix("\\\\?\\")
        .unwrap_or(&executable_text);
    let cwd_text = working_root.display().to_string();
    let cwd_text = cwd_text.strip_prefix("\\\\?\\").unwrap_or(&cwd_text);
    let path = wide(executable_text);
    let cwd = wide(cwd_text);
    let mut command = wide(&format!("\"{executable_text}\" {arguments}"));
    // Windows requires SystemRoot even for an explicit executable in a LowBox;
    // an all-empty block fails CreateProcess with ERROR_ENVVAR_NOT_FOUND.
    // Only this OS path and the isolated temp root are retained; no user/token
    // environment, PATH, proxy or command interpreter settings are inherited.
    let system_root = std::env::var("SystemRoot").map_err(|_| PluginError::SandboxUnavailable)?;
    if system_root.contains('\0') {
        return Err(PluginError::SandboxUnavailable);
    }
    let system_drive = std::path::Path::new(&system_root)
        .components()
        .next()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .ok_or(PluginError::SandboxUnavailable)?;
    let environment = wide(&format!("APPDATA={cwd_text}\0LOCALAPPDATA={cwd_text}\0SystemDrive={system_drive}\0SystemRoot={system_root}\0TEMP={cwd_text}\0TMP={cwd_text}\0USERPROFILE={cwd_text}\0windir={system_root}\0"));
    let mut info = PROCESS_INFORMATION::default();
    // CREATE_NO_WINDOW is omitted: for a redirected stdio AppContainer child it
    // is unnecessary and some environments fail initialization with it.
    if unsafe {
        CreateProcessW(
            path.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        )
    } == 0
    {
        return Err(unavailable("create_process"));
    }
    let process = Handle(info.hProcess);
    let thread = Handle(info.hThread);
    if unsafe { AssignProcessToJobObject(job.0, process.0) } == 0 {
        unsafe {
            TerminateProcess(process.0, 78);
        }
        return Err(unavailable("assign_job"));
    }
    if unsafe { ResumeThread(thread.0) } == u32::MAX {
        unsafe {
            TerminateJobObject(job.0, 78);
        }
        return Err(unavailable("resume_thread"));
    }
    // Hand File ownership to Rust without ever inheriting the parent's ends.
    let input = unsafe { File::from_raw_handle(stdin_write.0 as RawHandle) };
    stdin_write.0 = std::ptr::null_mut();
    let output = unsafe { File::from_raw_handle(stdout_read.0 as RawHandle) };
    stdout_read.0 = std::ptr::null_mut();
    // Closing the host copies of the child's ends makes EOF meaningful.
    unsafe {
        CloseHandle(stdin_read.0);
    }
    stdin_read.0 = std::ptr::null_mut();
    Ok(RestrictedProcess {
        process,
        job,
        input,
        output,
        profile_name,
    })
}
