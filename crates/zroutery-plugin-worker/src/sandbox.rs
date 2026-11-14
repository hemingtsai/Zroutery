#[cfg(target_os = "windows")]
pub fn established() -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TokenIsAppContainer, TOKEN_QUERY},
        System::{
            JobObjects::IsProcessInJob,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut app_container = 0u32;
    let mut length = 0;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenIsAppContainer,
            (&mut app_container as *mut u32).cast(),
            4,
            &mut length,
        )
    } != 0;
    unsafe {
        CloseHandle(token);
    }
    let mut in_job = 0;
    ok && app_container != 0
        && unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) } != 0
        && in_job != 0
}
#[cfg(not(target_os = "windows"))]
pub fn established() -> bool {
    false
}

#[cfg(target_os = "windows")]
pub fn probe(path: &str, parent: u32, port: u16) -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, PROCESS_VM_READ},
    };
    let file_denied = std::fs::read(path).is_err();
    let network_denied = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(200),
    )
    .is_err();
    let process = unsafe { OpenProcess(PROCESS_VM_READ, 0, parent) };
    let parent_denied = process.is_null();
    if !process.is_null() {
        unsafe {
            CloseHandle(process);
        }
    }
    let child_denied = std::process::Command::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/d", "/c", "exit", "0"])
        .spawn()
        .is_err();
    file_denied && network_denied && parent_denied && child_denied
}
