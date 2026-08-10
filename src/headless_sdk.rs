//! Headless SDK for RustDesk 鈥?provides a WebSocket-controlled,
//! UI-free remote desktop client for scripting/automation.
//!
//! Architecture:
//!   Python script 鈹€鈹€WebSocket鈹€鈹€鈻?headless_sdk 鈹€鈹€RustDesk protocol鈹€鈹€鈻?remote machine
//!
//! WebSocket endpoint: ws://127.0.0.1:9528/ws
//!
//! ## Protocol
//!
//! ### Commands (JSON Text frames, client 鈫?server)
//! ```json
//! {"id": 1, "cmd": "connect", "peer_id": "123456789", "password": "xxx"}
//! {"id": 2, "cmd": "disconnect"}
//! {"id": 3, "cmd": "screenshot"}
//! {"id": 4, "cmd": "mouse", "action": "click", "x": 500, "y": 300, "button": "left"}
//! {"id": 5, "cmd": "keyboard", "action": "key_down", "key": "W"}
//! {"id": 6, "cmd": "keyboard", "action": "key_up", "key": "W"}
//! {"id": 7, "cmd": "key_sequence", "keys_seq": [
//!   {"action": "key_down", "key": "MetaLeft", "delay_ms": 50},
//!   {"action": "key_click", "key": "2", "delay_ms": 80}
//! ]}
//! {"id": 8, "cmd": "get_id"}
//! {"id": 9, "cmd": "status"}
//! ```
//!
//! ### Responses (JSON Text frames, server 鈫?client)
//! ```json
//! {"id": 1, "ok": true, "state": "connecting"}
//! {"id": 3, "ok": true, "has_frame": true, "w": 1920, "h": 1080, "format": "abgr", "stride": 7680}
//! {"id": 8, "ok": true, "peer_id": "37513141"}
//! {"id": 9, "ok": true, "connected": true, "has_session": true}
//! ```
//! Image data follows as a **Binary frame**:
//! `[magic:u32 LE = 0x4B445352 "RSDK"][w:u32 LE][h:u32 LE][fmt:u32 LE][stride:u32 LE][raw_pixels:bytes]`
//!
//! ### Events (JSON Text frames, server 鈫?client, no id)
//! ```json
//! {"event": "connected"}
//! {"event": "disconnected", "reason": "connection lost"}
//! {"event": "error", "msg": "failed to connect"}
//! ```

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use futures::SinkExt;
use futures::StreamExt;
use hbb_common::tokio::io::AsyncBufReadExt;
use hbb_common::config;
use hbb_common::log;
use hbb_common::message_proto::{DisplayInfo, FileEntry, PeerInfo, SwitchDisplay, TerminalResponse, WindowsSession, CursorData, CursorPosition};
use hbb_common::rendezvous_proto::ConnType;
use hbb_common::tokio;
use hbb_common::tokio::sync::broadcast;
use scrap::ImageFormat;
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tungstenite::protocol::Message as WsMessage;

use librustdesk::client::{send_mouse, Interface, QualityStatus};
use librustdesk::ui_session_interface::{self, InvokeUiSession, Session};

/// Magic bytes for binary frame header: "RSDK" in LE.
const FRAME_MAGIC: [u8; 4] = *b"RSDK";

// 鈹€鈹€ Shared state 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

/// Stores the latest decoded video frame.
#[derive(Clone)]
struct LatestFrame {
    data: Vec<u8>,
    w: usize,
    h: usize,
    fmt: ImageFormat,
    stride: usize,
}

/// Events pushed from the session handler to WebSocket clients.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", content = "data")]
enum SdkEvent {
    Connected {
        direct: bool,
        secured: bool,
        stream: String,
    },
    Disconnected { reason: String },
    Error { msg: String },
}

impl SdkEvent {
    fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

// 鈹€鈹€ HeadlessHandler 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

/// Implements `InvokeUiSession` without any GUI.
#[derive(Clone, Default)]
struct HeadlessHandler {
    frame: Arc<Mutex<Option<LatestFrame>>>,
    displays: Arc<RwLock<Vec<DisplayInfo>>>,
    connected: Arc<AtomicBool>,
    event_tx: Arc<Mutex<Option<broadcast::Sender<SdkEvent>>>>,
    conn_info: Arc<Mutex<Option<(bool, bool, String)>>>,
}

impl HeadlessHandler {
    fn new(event_tx: broadcast::Sender<SdkEvent>) -> Self {
        Self {
            event_tx: Arc::new(Mutex::new(Some(event_tx))),
            ..Default::default()
        }
    }

    fn send_event(&self, evt: SdkEvent) {
        if let Some(tx) = self.event_tx.lock().unwrap().as_ref() {
            let _ = tx.send(evt);
        }
    }

    /// Compute stride (bytes per row) from raw buffer and dimensions.
    fn calc_stride(rgba: &scrap::ImageRgb) -> usize {
        if rgba.h == 0 {
            return 0;
        }
        rgba.raw.len() / rgba.h
    }
}

impl InvokeUiSession for HeadlessHandler {
    fn on_rgba(&self, _display: usize, rgba: &mut scrap::ImageRgb) {
        let frame = LatestFrame {
            stride: Self::calc_stride(rgba),
            data: rgba.raw.clone(),
            w: rgba.w,
            h: rgba.h,
            fmt: rgba.fmt,
        };
        *self.frame.lock().unwrap() = Some(frame);
    }

    fn on_connected(&self, conn_type: ConnType) {
        self.connected.store(true, Ordering::SeqCst);
        let (direct, secured, stream) = self
            .conn_info
            .lock()
            .unwrap()
            .clone()
            .unwrap_or((false, true, "unknown".into()));
        self.send_event(SdkEvent::Connected {
            direct,
            secured,
            stream,
        });
        log::info!("HeadlessSDK: connected, conn_type={conn_type:?}");
    }

    fn set_peer_info(&self, peer_info: &PeerInfo) {
        let (direct, secured, stream) = self
            .conn_info
            .lock()
            .unwrap()
            .clone()
            .unwrap_or((false, true, "unknown".into()));
        self.send_event(SdkEvent::Connected {
            direct,
            secured,
            stream,
        });
        log::info!(
            "HeadlessSDK: peer_info 鈥?version={}, username={}",
            peer_info.version,
            peer_info.username
        );
    }

    fn set_displays(&self, displays: &Vec<DisplayInfo>) {
        *self.displays.write().unwrap() = displays.clone();
        for d in displays {
            log::info!(
                "HeadlessSDK: display 鈥?{}x{} at ({},{})",
                d.width, d.height, d.x, d.y
            );
        }
    }

    fn close_success(&self) {
        self.connected.store(false, Ordering::SeqCst);
        self.send_event(SdkEvent::Disconnected {
            reason: "closed".to_string(),
        });
    }

    fn msgbox(&self, msgtype: &str, title: &str, text: &str, _link: &str, _retry: bool) {
        log::warn!("HeadlessSDK msgbox: [{msgtype}] {title}: {text}");
        if msgtype == "error" {
            self.connected.store(false, Ordering::SeqCst);
            let reason = format!("{title}: {text}");
            self.send_event(SdkEvent::Disconnected { reason });
        }
    }

    fn update_quality_status(&self, qs: QualityStatus) {
        log::debug!("QualityStatus: fps={:?}, delay={:?}", qs.fps, qs.delay);
    }

    fn set_connection_type(&self, is_secured: bool, direct: bool, stream_type: &str) {
        // Store as (direct, secured, stream) — matches reading order in on_connected / set_peer_info
        *self.conn_info.lock().unwrap() = Some((direct, is_secured, stream_type.to_string()));
        log::info!(
            "HeadlessSDK: connection 鈥?secured={is_secured}, direct={direct}, stream={stream_type}"
        );
    }

    fn get_rgba(&self, _display: usize) -> *const u8 {
        self.frame
            .lock()
            .unwrap()
            .as_ref()
            .map(|f| f.data.as_ptr())
            .unwrap_or(std::ptr::null())
    }

    fn next_rgba(&self, _display: usize) {}

    fn handle_screenshot_resp(&self, _sid: String, _msg: String) {
        log::debug!("screenshot response: {_msg}");
    }

    fn handle_terminal_response(&self, _response: TerminalResponse) {}

    // 鈹€鈹€ No-ops 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    fn set_cursor_data(&self, _cd: CursorData) {}
    fn set_cursor_id(&self, _id: String) {}
    fn set_cursor_position(&self, _cp: CursorPosition) {}
    fn set_display(&self, _x: i32, _y: i32, _w: i32, _h: i32, _ce: bool, _s: f64) {}
    fn switch_display(&self, _display: &SwitchDisplay) {}
    fn set_platform_additions(&self, _data: &str) {}
    fn update_privacy_mode(&self) {}
    fn set_permission(&self, _name: &str, _value: bool) {}
    fn set_fingerprint(&self, _fingerprint: String) {}
    fn job_error(&self, _id: i32, _err: String, _file_num: i32) {}
    fn job_done(&self, _id: i32, _file_num: i32) {}
    fn clear_all_jobs(&self) {}
    fn new_message(&self, _msg: String) {}
    fn update_transfer_list(&self) {}
    fn load_last_job(&self, _cnt: i32, _job_json: &str, _auto_start: bool) {}
    fn update_folder_files(
        &self, _id: i32, _entries: &Vec<FileEntry>,
        _path: String, _is_local: bool, _only_count: bool,
    ) {}
    fn confirm_delete_files(&self, _id: i32, _i: i32, _name: String) {}
    fn override_file_confirm(
        &self, _id: i32, _file_num: i32, _to: String, _is_upload: bool, _is_identical: bool,
    ) {}
    fn update_block_input_state(&self, _on: bool) {}
    fn job_progress(&self, _id: i32, _file_num: i32, _speed: f64, _finished_size: f64) {}
    fn adapt_size(&self) {}
    fn cancel_msgbox(&self, _tag: &str) {}
    fn switch_back(&self, _id: &str) {}
    fn portable_service_running(&self, _running: bool) {}
    fn on_voice_call_started(&self) {}
    fn on_voice_call_closed(&self, _reason: &str) {}
    fn on_voice_call_waiting(&self) {}
    fn on_voice_call_incoming(&self) {}
    fn set_multiple_windows_session(&self, _sessions: Vec<WindowsSession>) {}
    fn set_current_display(&self, _disp_idx: i32) {}
    fn update_record_status(&self, _start: bool) {}
    fn printer_request(&self, _id: i32, _path: String) {}
    #[cfg(all(feature = "vram", feature = "flutter"))]
    fn on_texture(&self, _display: usize, _texture: *mut std::ffi::c_void) {}
    #[cfg(any(target_os = "android", target_os = "ios"))]
    fn clipboard(&self, _content: String) {}
    #[cfg(feature = "flutter")]
    fn is_multi_ui_session(&self) -> bool { false }
}

// ─── Pipe mode (stdin/stdout) ─────────────────────────────────────────

pub fn run_pipe() {
    let rt = hbb_common::tokio::runtime::Runtime::new()
        .expect("failed to create tokio runtime");
    rt.block_on(async {
        let (event_tx, _) = broadcast::channel::<SdkEvent>(32);
        let mut event_rx = event_tx.subscribe();

        // Spawn a task to log events to stderr (keeps stdout clean for protocol)
        tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(SdkEvent::Error { msg }) => {
                        let evt = serde_json::json!({"event":"error","msg":msg});
                        eprintln!("{}", evt);
                    }
                    _ => {}
                }
            }
        });

        let state = AppState::new(
            HeadlessHandler::new(event_tx),
            broadcast::channel::<SdkEvent>(32).1,
        );

        let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
        let mut stdout = tokio::io::stdout();
        let mut line = String::new();

        log::info!("HeadlessSDK pipe mode ready, reading commands from stdin");

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {}
                Err(e) => {
                    log::error!("stdin read error: {e}");
                    break;
                }
            }
            let cmd = line.trim();
            if cmd.is_empty() {
                continue;
            }

            let (json_resp, binary_frame) = handle_text_command(&state, cmd).await;

            // Write binary BEFORE text so Python reads binary first,
            // then reads the JSON response line
            if let Some(bin) = binary_frame {
                use tokio::io::AsyncWriteExt;
                stdout.write_all(&bin).await.ok();
            }
            use tokio::io::AsyncWriteExt;
            let mut buf = json_resp.into_bytes();
            buf.push(b'\n');
            stdout.write_all(&buf).await.ok();
            stdout.flush().await.ok();
        }

        handle_disconnect(&state, 0);
        // Emit disconnect event so the caller knows the session is closed
        let evt = serde_json::json!({"event": "disconnected", "reason": "pipe closed"});
        stdout.write_all(evt.to_string().as_bytes()).await.ok();
        stdout.write_all(b"\n").await.ok();
        stdout.flush().await.ok();
        log::info!("HeadlessSDK pipe mode exiting");
    });
}

// ─── WebSocket server ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Command {
    #[serde(default)]
    id: u64,
    cmd: String,
    #[serde(default)]
    peer_id: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
    #[serde(default)]
    button: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default)]
    keys_seq: Vec<serde_json::Value>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    check_server: bool,
    // 鈹€鈹€ Video quality 鈹€鈹€
    #[serde(default)]
    quality: String,
    #[serde(default)]
    value: i32,
    #[serde(default)]
    fps: i32,
    #[serde(default)]
    display: i32,
    #[serde(default)]
    width: i32,
    #[serde(default)]
    height: i32,
}

struct AppState {
    session: Arc<RwLock<Option<Arc<Session<HeadlessHandler>>>>>,
    handler: HeadlessHandler,
    event_rx: Arc<Mutex<broadcast::Receiver<SdkEvent>>>,
}

impl AppState {
    fn new(handler: HeadlessHandler, event_rx: broadcast::Receiver<SdkEvent>) -> Self {
        Self {
            session: Arc::new(RwLock::new(None)),
            handler,
            event_rx: Arc::new(Mutex::new(event_rx)),
        }
    }

    fn get_session(&self) -> Option<Arc<Session<HeadlessHandler>>> {
        self.session.read().unwrap().clone()
    }
}

pub async fn run_server(addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(&addr).await?;
    log::info!("HeadlessSDK WebSocket server listening on ws://{addr}/ws");

    let (event_tx, _) = broadcast::channel::<SdkEvent>(32);

    while let Ok((stream, peer_addr)) = listener.accept().await {
        log::info!("New WebSocket connection from {peer_addr}");
        let state = AppState::new(
            HeadlessHandler::new(event_tx.clone()),
            event_tx.subscribe(),
        );
        tokio::spawn(handle_connection(stream, state));
    }

    Ok(())
}

async fn handle_connection(stream: TcpStream, state: AppState) {
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            log::error!("WebSocket handshake error: {e}");
            return;
        }
    };

    let (ws_sender, mut ws_receiver) = ws_stream.split();

    // Forward events to WebSocket client
    let event_rx = state.event_rx.clone();
    let ws_sender_events = Arc::new(tokio::sync::Mutex::new(ws_sender));
    let ws_sender_clone = ws_sender_events.clone();

    let event_forward_handle = tokio::spawn(async move {
        let mut rx = event_rx.lock().unwrap().resubscribe();
        loop {
            match rx.recv().await {
                Ok(evt) => {
                    let msg = WsMessage::Text(evt.to_json_string().into());
                    if ws_sender_clone.lock().await.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("Event channel lagged by {n}, resubscribing");
                    rx = rx.resubscribe();
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Process commands
    while let Some(msg) = ws_receiver.next().await {
        match msg {
            Ok(WsMessage::Text(text)) => {
                let (json_resp, binary_frame) = handle_text_command(&state, &text).await;
                let mut sender = ws_sender_events.lock().await;
                // Send binary BEFORE text so Python always buffers binary first,
                // then picks it up when the JSON response arrives.
                if let Some(bin) = binary_frame {
                    let _ = sender.send(WsMessage::Binary(bin.into())).await;
                }
                let _ = sender.send(WsMessage::Text(json_resp.into())).await;
            }
            Ok(WsMessage::Close(_)) | Ok(WsMessage::Ping(_)) | Ok(WsMessage::Pong(_)) => {}
            Ok(WsMessage::Binary(_)) | Ok(WsMessage::Frame(_)) => {}
            Err(e) => {
                log::error!("WebSocket error: {e}");
                break;
            }
        }
    }

    event_forward_handle.abort();

    // Disconnect remote session when WebSocket closes
    handle_disconnect(&state, 0);
    log::info!("WebSocket connection closed, remote session cleaned up");
}

/// Returns (json_response, optional_binary_frame)
async fn handle_text_command(state: &AppState, text: &str) -> (String, Option<Vec<u8>>) {
    let cmd: Command = match serde_json::from_str(text) {
        Ok(c) => c,
        Err(e) => return (json_error(0, &format!("invalid command: {e}")), None),
    };

    match cmd.cmd.as_str() {
        "connect" => (handle_connect(state, &cmd).await, None),
        "disconnect" => (handle_disconnect(state, cmd.id), None),
        "screenshot" => handle_screenshot(cmd.id, state),
        "mouse" => (handle_mouse(state, &cmd).await, None),
        "keyboard" => (handle_keyboard(state, &cmd).await, None),
        "key_sequence" => (handle_key_sequence(state, &cmd).await, None),
        "get_id" => (handle_get_id(cmd.id), None),
        "set_id" => (handle_set_id(cmd.id, &cmd).await, None),
        "set_quality" => (handle_set_quality(state, &cmd), None),
        "set_fps" => (handle_set_fps(state, &cmd), None),
        "set_resolution" => (handle_set_resolution(state, &cmd), None),
        "ping" => (json_ok(cmd.id), None),
        "status" => (handle_status(cmd.id, state), None),
        _ => (json_error(cmd.id, &format!("unknown command: {}", cmd.cmd)), None),
    }
}

// 鈹€鈹€ Command handlers 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

async fn handle_connect(state: &AppState, cmd: &Command) -> String {
    let peer_id = cmd.peer_id.clone();
    let password = cmd.password.clone();

    if peer_id.is_empty() {
        return json_error(cmd.id, "peer_id is required");
    }

    // Disconnect existing and wait for clean exit
    handle_disconnect(state, 0);
    let mut wait = 0;
    while state.handler.connected.load(Ordering::SeqCst) && wait < 30 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        wait += 1;
    }
    log::info!("handle_connect: old session cleaned up after {}ms", wait * 200);

    let session: Session<HeadlessHandler> = Session {
        password,
        ui_handler: state.handler.clone(),  // share frame buffer with screenshot handler
        server_keyboard_enabled: Arc::new(RwLock::new(true)),
        server_file_transfer_enabled: Arc::new(RwLock::new(true)),
        server_clipboard_enabled: Arc::new(RwLock::new(true)),
        reconnect_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        ..Default::default()
    };

    session.lc.write().unwrap().initialize(
        peer_id.clone(),
        ConnType::DEFAULT_CONN,
        None,
        false,
        None,
        None,
        None,
    );

    let session = Arc::new(session);
    let session_clone = session.clone();

    // io_loop has #[tokio::main] 鈥?it's a blocking function
    std::thread::spawn(move || {
        ui_session_interface::io_loop((*session_clone).clone(), 0);
        log::info!("io_loop exited");
    });

    *state.session.write().unwrap() = Some(session);

    serde_json::json!({"id": cmd.id, "ok": true, "state": "connecting"}).to_string()
}

fn handle_disconnect(state: &AppState, id: u64) -> String {
    if let Some(session) = state.session.read().unwrap().as_ref() {
        let data = librustdesk::client::Data::Close;
        Interface::send(session.as_ref(), data);
    }
    *state.session.write().unwrap() = None;
    json_ok(id)
}

fn handle_status(id: u64, state: &AppState) -> String {
    let connected = state.handler.connected.load(Ordering::SeqCst);
    let has_session = state.session.read().unwrap().is_some();
    serde_json::json!({
        "id": id,
        "ok": true,
        "connected": connected,
        "has_session": has_session,
    }).to_string()
}

fn handle_screenshot(id: u64, state: &AppState) -> (String, Option<Vec<u8>>) {
    let frame = state.handler.frame.lock().unwrap();
    match frame.as_ref() {
        Some(f) => {
            let fmt_code: u32 = match f.fmt {
                ImageFormat::ABGR => 0,
                ImageFormat::ARGB => 1,
                ImageFormat::Raw => 2,
            };

            let response = serde_json::json!({
                "id": id,
                "ok": true,
                "has_frame": true,
                "w": f.w,
                "h": f.h,
                "format": format_code(f.fmt),
                "stride": f.stride,
                "fmt_code": fmt_code,
            });

            // Build binary header: [magic:u32 LE][w:u32 LE][h:u32 LE][fmt:u32 LE][stride:u32 LE]
            let mut header = Vec::with_capacity(20);
            header.extend_from_slice(&FRAME_MAGIC);
            header.extend_from_slice(&(f.w as u32).to_le_bytes());
            header.extend_from_slice(&(f.h as u32).to_le_bytes());
            header.extend_from_slice(&fmt_code.to_le_bytes());
            header.extend_from_slice(&(f.stride as u32).to_le_bytes());

            let mut binary = header;
            binary.extend_from_slice(&f.data);

            (response.to_string(), Some(binary))
        }
        None => {
            let response = serde_json::json!({
                "id": id,
                "ok": true,
                "has_frame": false,
            });
            (response.to_string(), None)
        }
    }
}

fn format_code(fmt: ImageFormat) -> &'static str {
    match fmt {
        ImageFormat::ABGR => "abgr",
        ImageFormat::ARGB => "argb",
        ImageFormat::Raw => "rgb",
    }
}

async fn handle_mouse(state: &AppState, cmd: &Command) -> String {
    let Some(session) = state.get_session() else {
        log::warn!("handle_mouse: no session");
        return json_error(cmd.id, "not connected");
    };
    log::info!("handle_mouse: action={} x={} y={} button={}", cmd.action, cmd.x, cmd.y, cmd.button);

    use librustdesk::common::input::{
        MOUSE_BUTTON_BACK, MOUSE_BUTTON_FORWARD, MOUSE_BUTTON_LEFT, MOUSE_BUTTON_RIGHT,
        MOUSE_BUTTON_WHEEL, MOUSE_TYPE_DOWN, MOUSE_TYPE_MOVE_RELATIVE, MOUSE_TYPE_UP,
        MOUSE_TYPE_WHEEL,
    };

    let button_mask = match cmd.button.as_str() {
        "left" => MOUSE_BUTTON_LEFT,
        "right" => MOUSE_BUTTON_RIGHT,
        "middle" => MOUSE_BUTTON_WHEEL,
        "back" => MOUSE_BUTTON_BACK,
        "forward" => MOUSE_BUTTON_FORWARD,
        _ => MOUSE_BUTTON_LEFT,
    };

    // Server reads buttons as `mask >> 3`, so shift button bits up.
    let make_mask = |mtype: i32| -> i32 { mtype | (button_mask << 3) };
    // Minimal protocol delay to prevent back-to-back messages merging
    const GAP: std::time::Duration = std::time::Duration::from_millis(10);

    match cmd.action.as_str() {
        // 鈹€鈹€ Pure primitives (Python orchestrates timing) 鈹€鈹€
        "click" => {
            send_mouse(make_mask(MOUSE_TYPE_DOWN), 0, 0, false, false, false, false, &*session);
            tokio::time::sleep(GAP).await;
            send_mouse(make_mask(MOUSE_TYPE_UP), 0, 0, false, false, false, false, &*session);
        }
        "down" => {
            send_mouse(make_mask(MOUSE_TYPE_DOWN), 0, 0, false, false, false, false, &*session);
        }
        "up" => {
            send_mouse(make_mask(MOUSE_TYPE_UP), 0, 0, false, false, false, false, &*session);
        }
        "move_to" => {
            send_mouse(0, cmd.x, cmd.y, false, false, false, false, &*session);
        }
        "move_relative" => {
            // (dx, dy) clamped to 卤10000 by server. Use for FPS-style mouse look.
            send_mouse(MOUSE_TYPE_MOVE_RELATIVE, cmd.x, cmd.y, false, false, false, false, &*session);
        }
        "scroll" => {
            // Server reads x/y from event coordinates, not from button bits.
            // WHEEL_DELTA=120 on Windows, so 1 unit = 120 at OS level.
            if cmd.x != 0 {
                send_mouse(MOUSE_TYPE_WHEEL, cmd.x, 0, false, false, false, false, &*session);
            }
            if cmd.y != 0 {
                send_mouse(MOUSE_TYPE_WHEEL, 0, cmd.y, false, false, false, false, &*session);
            }
        }
        _ => return json_error(cmd.id, &format!("unknown mouse action: {}", cmd.action)),
    }

    json_ok(cmd.id)
}

async fn handle_keyboard(state: &AppState, cmd: &Command) -> String {
    let Some(session) = state.get_session() else {
        log::warn!("handle_keyboard: no session");
        return json_error(cmd.id, "not connected");
    };
    log::info!("handle_keyboard: action={} key={}", cmd.action, cmd.key);

    let Some(sc) = name_to_scancode(&cmd.key) else {
        return json_error(cmd.id, &format!("unknown key: {}", cmd.key));
    };

    match cmd.action.as_str() {
        "key_down" => session.input_key_map(sc, true),
        "key_up" => session.input_key_map(sc, false),
        "key_click" => {
            session.input_key_map(sc, true);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            session.input_key_map(sc, false);
        }
        _ => return json_error(cmd.id, &format!("unknown keyboard action: {}", cmd.action)),
    }

    json_ok(cmd.id)
}

/// Map key names to Windows scan codes (Set 1).
/// Extended keys (arrows, nav, right modifiers, Win keys) use 0xE0xx format.
fn name_to_scancode(name: &str) -> Option<u32> {
    match name {
        // Letters (US keyboard layout)
        "a" | "A" => Some(0x1E), "b" | "B" => Some(0x30), "c" | "C" => Some(0x2E),
        "d" | "D" => Some(0x20), "e" | "E" => Some(0x12), "f" | "F" => Some(0x21),
        "g" | "G" => Some(0x22), "h" | "H" => Some(0x23), "i" | "I" => Some(0x17),
        "j" | "J" => Some(0x24), "k" | "K" => Some(0x25), "l" | "L" => Some(0x26),
        "m" | "M" => Some(0x32), "n" | "N" => Some(0x31), "o" | "O" => Some(0x18),
        "p" | "P" => Some(0x19), "q" | "Q" => Some(0x10), "r" | "R" => Some(0x13),
        "s" | "S" => Some(0x1F), "t" | "T" => Some(0x14), "u" | "U" => Some(0x16),
        "v" | "V" => Some(0x2F), "w" | "W" => Some(0x11), "x" | "X" => Some(0x2D),
        "y" | "Y" => Some(0x15), "z" | "Z" => Some(0x2C),
        // Digits (name 鈫?digit char 鈫?scancode)
        "0" => Some(0x0B), "1" => Some(0x02), "2" => Some(0x03), "3" => Some(0x04),
        "4" => Some(0x05), "5" => Some(0x06), "6" => Some(0x07), "7" => Some(0x08),
        "8" => Some(0x09), "9" => Some(0x0A),
        // Modifiers
        "MetaLeft" | "Meta" => Some(0xE05B),
        "MetaRight" | "RWin" => Some(0xE05C),
        "ControlLeft" | "Control" | "VK_CONTROL" => Some(0x1D),
        "ControlRight" => Some(0xE01D),
        "ShiftLeft" | "Shift" | "VK_SHIFT" => Some(0x2A),
        "ShiftRight" => Some(0x36),
        "AltLeft" | "Alt" | "VK_MENU" => Some(0x38),
        "AltRight" | "AltGr" => Some(0xE038),
        // Navigation & editing
        "Enter" | "Return" | "VK_RETURN" => Some(0x1C),
        "Space" | "VK_SPACE" => Some(0x39),
        "Tab" | "VK_TAB" => Some(0x0F),
        "Escape" | "Esc" | "VK_ESCAPE" => Some(0x01),
        "Backspace" | "VK_BACK" => Some(0x0E),
        "Delete" | "Del" | "VK_DELETE" => Some(0xE053),
        "Insert" | "Ins" | "VK_INSERT" => Some(0xE052),
        "Home" | "VK_HOME" => Some(0xE047),
        "End" | "VK_END" => Some(0xE04F),
        "PageUp" | "VK_PRIOR" => Some(0xE049),
        "PageDown" | "VK_NEXT" => Some(0xE051),
        "ArrowUp" | "Up" | "VK_UP" => Some(0xE048),
        "ArrowDown" | "Down" | "VK_DOWN" => Some(0xE050),
        "ArrowLeft" | "Left" | "VK_LEFT" => Some(0xE04B),
        "ArrowRight" | "Right" | "VK_RIGHT" => Some(0xE04D),
        "PrintScreen" | "VK_SNAPSHOT" => Some(0xE037),
        "CapsLock" | "VK_CAPITAL" => Some(0x3A),
        "ScrollLock" | "VK_SCROLL" => Some(0x46),
        "Pause" | "VK_PAUSE" => Some(0xE045),
        "Apps" | "Menu" => Some(0xE05D),
        // Function keys
        "F1" | "VK_F1" => Some(0x3B), "F2" | "VK_F2" => Some(0x3C),
        "F3" | "VK_F3" => Some(0x3D), "F4" | "VK_F4" => Some(0x3E),
        "F5" | "VK_F5" => Some(0x3F), "F6" | "VK_F6" => Some(0x40),
        "F7" | "VK_F7" => Some(0x41), "F8" | "VK_F8" => Some(0x42),
        "F9" | "VK_F9" => Some(0x43), "F10" | "VK_F10" => Some(0x44),
        "F11" | "VK_F11" => Some(0x57), "F12" | "VK_F12" => Some(0x58),
        // Numpad
        "NumLock" | "VK_NUMLOCK" => Some(0x45),
        "Numpad0" => Some(0x52), "Numpad1" => Some(0x4F), "Numpad2" => Some(0x50),
        "Numpad3" => Some(0x51), "Numpad4" => Some(0x4B), "Numpad5" => Some(0x4C),
        "Numpad6" => Some(0x4D), "Numpad7" => Some(0x47), "Numpad8" => Some(0x48),
        "Numpad9" => Some(0x49),
        "NumpadMultiply" | "Multiply" => Some(0x37),
        "NumpadAdd" | "Add" => Some(0x4E),
        "NumpadSubtract" | "Subtract" => Some(0x4A),
        "NumpadDecimal" | "Decimal" => Some(0x53),
        "NumpadDivide" | "Divide" => Some(0xE035),
        // Punctuation
        "-" | "Minus" => Some(0x0C),
        "=" | "Equal" => Some(0x0D),
        "[" | "BracketLeft" => Some(0x1A),
        "]" | "BracketRight" => Some(0x1B),
        ";" | "Semicolon" => Some(0x27),
        "'" | "Quote" => Some(0x28),
        "," | "Comma" => Some(0x33),
        "." | "Period" => Some(0x34),
        "/" | "Slash" => Some(0x35),
        "\\" | "Backslash" => Some(0x2B),
        "`" | "Backtick" => Some(0x29),
        // Convert Key0-Key9 format
        n if n.starts_with("Key") && n.len() == 4 => {
            name_to_scancode(&n[3..]) // recurse with just the digit
        }
        // Single char fallback 鈥?only used if not matched above
        _ => None,
    }
}

/// Execute a key sequence using Map mode (raw scan codes).
///
/// Each step: `{"action": "key_down"|"key_up"|"key_click"|"wait", "key": "...", "delay_ms": N}`
///
/// Map mode sends raw scan codes directly 鈥?no modifier syncing.
/// The remote OS tracks modifier state naturally from the key down/up stream.
async fn handle_key_sequence(state: &AppState, cmd: &Command) -> String {
    let Some(session) = state.get_session() else {
        log::warn!("handle_key_sequence: no session");
        return json_error(cmd.id, "not connected");
    };
    log::info!("handle_key_sequence: {} steps", cmd.keys_seq.len());

    const CLICK_GAP: std::time::Duration = std::time::Duration::from_millis(50);

    for (i, item) in cmd.keys_seq.iter().enumerate() {
        let action = item["action"].as_str().unwrap_or("");
        let key = item["key"].as_str().unwrap_or("");
        let delay_ms = item["delay_ms"].as_u64().unwrap_or(0);

        if action == "wait" {
            let ms = item["delay_ms"].as_u64().unwrap_or(100);
            log::info!("  key_sequence[{i}]: wait {ms}ms");
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            continue;
        }

        if action == "key_click" {
            // key_click = key_down + 50ms gap + key_up
            let Some(sc) = name_to_scancode(key) else {
                log::warn!("  key_sequence[{i}]: unknown key '{key}' for key_click");
                continue;
            };
            log::info!("  key_sequence[{i}]: key_click({key}) sc=0x{sc:04X}");
            session.input_key_map(sc, true);
            tokio::time::sleep(CLICK_GAP).await;
            session.input_key_map(sc, false);
            let ms = if delay_ms > 0 { delay_ms } else { 50 };
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            continue;
        }

        if key.is_empty() {
            log::warn!("  key_sequence[{i}]: empty key for action '{action}', skipping");
            continue;
        }

        let Some(sc) = name_to_scancode(key) else {
            log::warn!("  key_sequence[{i}]: unknown key '{key}'");
            continue;
        };

        let down = match action {
            "key_down" => true,
            "key_up" => false,
            _ => {
                log::warn!("  key_sequence[{i}]: unknown action '{action}'");
                continue;
            }
        };

        log::info!("  key_sequence[{i}]: {action}({key}) sc=0x{sc:04X} delay={delay_ms}ms");
        session.input_key_map(sc, down);
        let ms = if delay_ms > 0 {
            delay_ms
        } else if down {
            40
        } else {
            30
        };
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }

    json_ok(cmd.id)
}

// 鈹€鈹€ Video quality 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

fn handle_set_quality(state: &AppState, cmd: &Command) -> String {
    let Some(session) = state.get_session() else {
        return json_error(cmd.id, "not connected");
    };
    let quality = if cmd.quality.is_empty() {
        "balanced"
    } else {
        &cmd.quality
    };
    match quality {
        "best" | "balanced" | "low" => {
            log::info!("handle_set_quality: preset={}", quality);
            session.save_image_quality(quality.to_string());
        }
        "custom" => {
            let v = if cmd.value > 0 { cmd.value } else { 50 };
            log::info!("handle_set_quality: custom value={}", v);
            session.save_custom_image_quality(v);
        }
        _ => return json_error(cmd.id, &format!("unknown quality: {quality} (use best/balanced/low/custom)")),
    }
    json_ok(cmd.id)
}

fn handle_set_fps(state: &AppState, cmd: &Command) -> String {
    let Some(session) = state.get_session() else {
        return json_error(cmd.id, "not connected");
    };
    let fps = if cmd.fps < 1 { 30 } else { cmd.fps };
    log::info!("handle_set_fps: fps={}", fps);
    session.set_custom_fps(fps);
    json_ok(cmd.id)
}

fn handle_set_resolution(state: &AppState, cmd: &Command) -> String {
    let Some(session) = state.get_session() else {
        return json_error(cmd.id, "not connected");
    };
    let display = cmd.display;
    let w = if cmd.width > 0 { cmd.width } else { 1920 };
    let h = if cmd.height > 0 { cmd.height } else { 1080 };
    log::info!("handle_set_resolution: display={display} {w}x{h}");
    session.change_resolution(display, w, h);
    json_ok(cmd.id)
}

// 鈹€鈹€ ID management 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

fn handle_get_id(id: u64) -> String {
    let peer_id = config::Config::get_id();
    serde_json::json!({"id": id, "ok": true, "peer_id": peer_id}).to_string()
}

/// Set the local peer ID.
///
/// Without `check_server` (default): writes directly via `Config::set_id()`,
/// no format validation, no server check. Pure-numeric IDs are allowed.
/// **The new ID takes effect on the next restart / connection.**
///
/// With `check_server: true`: validates format (`^[a-zA-Z][\\w-]{5,15}$`)
/// and checks availability on the configured rendezvous server. Fails if:
/// - Format is invalid
/// - ID is already taken on the server
/// - Change is too frequent
async fn handle_set_id(id: u64, cmd: &Command) -> String {
    let new_id = cmd.peer_id.clone();
    if new_id.is_empty() {
        return json_error(id, "peer_id is required");
    }

    if cmd.check_server {
        // Full validation path: format check + server uniqueness check
        let old_id = config::Config::get_id();
        let err = librustdesk::ui_interface::change_id_shared_(new_id.clone(), old_id).await;
        if err.is_empty() {
            let current = config::Config::get_id();
            serde_json::json!({"id": id, "ok": true, "peer_id": current}).to_string()
        } else {
            json_error(id, err)
        }
    } else {
        // Direct path: no validation, no server check
        if new_id != config::Config::get_id() {
            let config_path = config::Config::file();

            // 1) Normal write (encrypted enc_id) 鈥?for headless_sdk itself
            config::Config::set_id(&new_id);

            // 2) Compatibility write (plain id, no enc_id) 鈥?for official RustDesk
            //    The official binary may use a different get_uuid() impl,
            //    causing enc_id decryption to fail. Writing plain id bypasses this.
            let mut raw =
                hbb_common::config::load_path::<config::Config>(config_path.clone());
            raw.id = new_id.clone();
                        if let Err(e) =
                hbb_common::config::store_path(config_path, &raw)
            {
                log::error!("set_id: compatibility write failed: {e}");
            }
        }
        let current = config::Config::get_id();
        serde_json::json!({"id": id, "ok": true, "peer_id": current}).to_string()
    }
}

// 鈹€鈹€ JSON helpers 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

fn json_ok(id: u64) -> String {
    serde_json::json!({"id": id, "ok": true}).to_string()
}

fn json_error(id: u64, msg: &str) -> String {
    serde_json::json!({"id": id, "ok": false, "error": msg}).to_string()
}
