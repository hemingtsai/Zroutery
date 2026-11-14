//! No package byte is read until the OS token/job boundary has been established.
mod sandbox;
fn main() {
    if !sandbox::established() {
        std::process::exit(78);
    }
    #[cfg(target_os = "windows")]
    if std::env::args().nth(1).as_deref() == Some("--sandbox-probe") {
        let args: Vec<_> = std::env::args().collect();
        let good =
            args.get(2)
                .zip(args.get(3))
                .zip(args.get(4))
                .is_some_and(|((path, parent), port)| {
                    parent
                        .parse::<u32>()
                        .ok()
                        .zip(port.parse::<u16>().ok())
                        .is_some_and(|(parent, port)| sandbox::probe(path, parent, port))
                });
        std::process::exit(if good { 0 } else { 79 });
    }
    use zroutery_plugin_api::ipc::{
        read_frame, write_frame, Envelope, WorkerBootstrap, WorkerCall, WorkerReply,
    };
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let Ok(bootstrap) = read_frame::<WorkerBootstrap>(&mut input) else {
        std::process::exit(78);
    };
    let Ok(call) = read_frame::<Envelope<WorkerCall>>(&mut input) else {
        std::process::exit(78);
    };
    if call.ipc_version != 1 {
        std::process::exit(78);
    }
    let value = zroutery_plugin_worker::runtime::execute(&bootstrap, &call.value)
        .unwrap_or_else(|code| WorkerReply::Error { code });
    let reply = Envelope {
        ipc_version: 1,
        call_id: call.call_id,
        worker_generation: call.worker_generation,
        grant_epoch: call.grant_epoch,
        value,
    };
    if write_frame(&mut output, &reply).is_err() {
        std::process::exit(78);
    }
}
