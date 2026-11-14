//! Isolated OS negative fixture. Never reads a user's config or credential store.
#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(worker) = args.get(1) else {
        std::process::exit(2);
    };
    let Some(root) = args.get(2) else {
        std::process::exit(2);
    };
    let root = std::path::PathBuf::from(root);
    std::fs::create_dir_all(&root).unwrap();
    let secret = root.join("host-canary.txt");
    std::fs::write(&secret, "synthetic-host-only-canary").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let work = root.join("worker-root");
    std::fs::create_dir_all(&work).unwrap();
    let arguments = format!(
        "--sandbox-probe {} {} {}",
        secret.display(),
        std::process::id(),
        port
    );
    match zroutery_plugin_host::sandbox::launch(std::path::Path::new(worker), &work, &arguments) {
        Ok(process) => match process.wait(5000) {
            Ok(0) => println!("sandbox-probe: PASS file/network/parent/child negatives"),
            Ok(code) => {
                eprintln!("sandbox-probe: FAILED exit={code}");
                std::process::exit(1);
            }
            Err(code) => {
                eprintln!("sandbox-probe: FAILED {code}");
                std::process::exit(1);
            }
        },
        Err(code) => {
            eprintln!("sandbox-probe: UNAVAILABLE {code}; executable facets remain denied");
            std::process::exit(2);
        }
    }
}
#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("sandbox-probe: UNAVAILABLE native platform evidence required");
    std::process::exit(2);
}
