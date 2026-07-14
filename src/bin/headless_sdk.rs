//! Headless SDK binary for RustDesk.
//!
//! Provides a WebSocket-controlled, UI-free remote desktop client.
//!
//! Usage: `headless_sdk.exe [--port 9528] [--host 127.0.0.1]`
//! WebSocket endpoint: ws://127.0.0.1:9528/ws

use std::net::SocketAddr;

#[path = "../headless_sdk.rs"]
mod headless_sdk;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !librustdesk::common::global_init() {
        eprintln!("Global initialization failed.");
        std::process::exit(1);
    }

    hbb_common::init_log(false, "headless_sdk");

    // Ensure config is loaded
    let _ = hbb_common::config::Config::get_id();

    hbb_common::log::info!("HeadlessSDK v{} starting...", librustdesk::VERSION);

    let args: Vec<String> = std::env::args().collect();
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 9528;
    let mut use_pipe = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--pipe" => {
                use_pipe = true;
            }
            "--port" => {
                i += 1;
                if i < args.len() {
                    port = args[i].parse().unwrap_or(9528);
                }
            }
            "--host" => {
                i += 1;
                if i < args.len() {
                    host = args[i].clone();
                }
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    if use_pipe {
        hbb_common::log::info!("HeadlessSDK v{} starting in pipe mode...", librustdesk::VERSION);
        headless_sdk::run_pipe();
    } else {
        let addr: SocketAddr = format!("{host}:{port}").parse()?;
        hbb_common::log::info!("HeadlessSDK listening on ws://{addr}/ws");
        let rt = hbb_common::tokio::runtime::Runtime::new()?;
        rt.block_on(headless_sdk::run_server(addr))?;
    }

    librustdesk::common::global_clean();
    Ok(())
}

fn print_usage() {
    println!(
        "HeadlessSDK - RustDesk automation backend\n\n\
         Usage:\n  headless_sdk.exe [OPTIONS]\n\n\
         Options:\n  \
         --pipe        Use stdin/stdout pipe mode (Python subprocess)\n  \
         --port PORT   WebSocket server port (default: 9528)\n  \
         --host HOST   Bind address (default: 127.0.0.1)\n  \
         --help, -h    Show this help\n\n\
         Pipe mode:\n  \
         Commands via stdin (one JSON per line), responses via stdout.\n  \
         See src/headless_sdk.rs for JSON command format.\n\n\
         WebSocket mode:\n  \
         Connect to ws://HOST:PORT/ws\n"
    );
}
