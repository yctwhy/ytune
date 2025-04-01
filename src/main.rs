#![windows_subsystem = "windows"]

mod discord_ipc;

use std::{
    fs::File,
    io,
    process,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wry::{
    application::event_loop::{ControlFlow, EventLoop},
    application::window::{Icon, Window, WindowBuilder},
    webview::WebViewBuilder,
};
use image::{load_from_memory_with_format, ImageFormat};

#[cfg(target_os = "windows")]
use crate::discord_ipc::{connect, read_message, send_handshake, set_activity};

#[cfg(target_os = "windows")]
const CLIENT_ID: &str = "1356377176563384371";
const INIT_JS: &str = r#"

    function getElementByXpath(path) {
        try {
            return document.evaluate(path, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue;
        } catch (e) { return null; }
    }

    function parseTimeToSeconds(timeStr) {
        if (!timeStr || typeof timeStr !== 'string') return null;

        const parts = timeStr.split(':').map(Number);
        if (parts.some(isNaN) || parts.length > 3 || parts.length < 1) return null;

        let seconds = 0;
        if (parts.length === 3) {
            seconds = parts[0] * 3600 + parts[1] * 60 + parts[2];
        } else if (parts.length === 2) {
            seconds = parts[0] * 60 + parts[1];
        } else {
            seconds = parts[0];
        }

        return seconds > 0 ? seconds : null;
    }

    function getTrackInfo() {
        const playerBar = document.querySelector('ytmusic-player-bar');
        if (!playerBar) return;

        const titleEl = playerBar.querySelector('.title.style-scope.ytmusic-player-bar');
        const artistContainer = playerBar.querySelector('.byline.style-scope.ytmusic-player-bar');
        const albumArtEl = playerBar.querySelector('img');
        const durationEl = playerBar.querySelector('#progress-bar .time-info.style-scope.ytmusic-player-bar');

        const titleText = titleEl?.innerText.trim() || "";
        let artistText = "";
        if (artistContainer) {
            const artistNodes = Array.from(artistContainer.querySelectorAll('yt-formatted-string a, yt-formatted-string'));
            artistText = artistNodes.map(node => node.innerText.trim()).filter(text => text && !/^\d{4}$/.test(text)).join(', ');
            artistText = artistText ? artistText.trim() : "";
        }

        const albumArtUrl = albumArtEl?.getAttribute("src") || "";

        let durationSeconds = null;
        if (durationEl && durationEl.innerText) {
            const timeText = durationEl.innerText.split('/').pop().trim();
            if (timeText && timeText.toLowerCase() !== "live") {
                durationSeconds = parseTimeToSeconds(timeText);
            }
        }

        const cleanedTitle = titleText ? titleText.split(' • ')[0].trim() : "";
        const cleanedArtist = artistText ? artistText.split(' • ')[0].trim() : "";

        if (cleanedTitle || cleanedArtist) {

            window.ipc.postMessage(JSON.stringify({
                cmd: 'trackUpdate',
                title: cleanedTitle || null,
                artist: cleanedArtist || null,
                album_art: albumArtUrl || null,
                duration: durationSeconds
            }));
        }
    }

    setInterval(getTrackInfo, 5000);

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', () => setTimeout(getTrackInfo, 1500));
    } else {
         setTimeout(getTrackInfo, 1500);
    }
"#;

#[cfg(target_os = "windows")]
type DiscordConnectionState = Option<(File, u32)>;

#[derive(Clone, PartialEq, Debug, Default)]
struct LastTrackInfo {
    title: Option<String>,
    artist: Option<String>,
    album_art: Option<String>,
    duration_sec: Option<u64>,
}

fn main() -> wry::Result<()> {

    let window_icon = load_window_icon();

    #[cfg(target_os = "windows")]
    let discord_connection: Arc<Mutex<DiscordConnectionState>> = Arc::new(Mutex::new(None));
    let last_track = Arc::new(Mutex::new(LastTrackInfo::default()));

    #[cfg(target_os = "windows")]
    {
        let conn_arc_clone = Arc::clone(&discord_connection);
        let client_id_clone = CLIENT_ID.to_string();

        thread::spawn(move || {
            let pid = process::id();
            match connect_and_handshake(&client_id_clone, pid) {
                Ok(file) => {

                    let mut guard = conn_arc_clone.lock().unwrap();
                    *guard = Some((file, pid));
                }
                Err(e) => {
                    eprintln!(
                        "Initial Discord connection failed: {:?}. Will retry on track update.",
                        e
                    );
                }
            }
        });
    }

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("ytune")
        .with_window_icon(window_icon)
        .build(&event_loop)?;

    #[cfg(target_os = "windows")]
    let conn_arc_clone_ipc = Arc::clone(&discord_connection);
    let last_track_clone = Arc::clone(&last_track);

    let _webview = WebViewBuilder::new(window)?
        .with_url("https://music.youtube.com")?
        .with_initialization_script(INIT_JS)
        .with_ipc_handler(move |_window: &Window, req: String| {

            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&req) {
                if obj.get("cmd").and_then(|v| v.as_str()) == Some("trackUpdate") {

                    let current_track = LastTrackInfo {
                        title: obj.get("title").and_then(|v| v.as_str()).map(str::to_string),
                        artist: obj.get("artist").and_then(|v| v.as_str()).map(str::to_string),
                        album_art: obj.get("album_art").and_then(|v| v.as_str()).map(str::to_string),
                        duration_sec: obj.get("duration").and_then(|v| v.as_u64()),
                    };

                    let should_update_discord;
                    {

                        let mut last_track_guard = last_track_clone.lock().unwrap();
                        should_update_discord = *last_track_guard != current_track;
                        *last_track_guard = current_track.clone();
                    }

                    if should_update_discord {
                        #[cfg(target_os = "windows")]
                        {
                            let clean_title = current_track.title.as_deref().unwrap_or("");
                            let clean_artist = current_track.artist.as_deref().unwrap_or("");
                            let clean_album_art = current_track.album_art.as_deref().unwrap_or("");

                            if clean_title.is_empty() && clean_artist.is_empty() {
                                return;
                            }

                            let start_time = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();

                            let end_time = current_track.duration_sec.map(|d| start_time + d);
                            let timestamp_json = if let Some(end) = end_time {
                                serde_json::json!({ "start": start_time, "end": end })
                            } else {
                                serde_json::json!({ "start": start_time })
                            };

                            let activity_payload = serde_json::json!({
                                "timestamps": timestamp_json,
                                "assets": {
                                    "large_image": if clean_album_art.is_empty() { serde_json::Value::Null } else { clean_album_art.into() },
                                    "small_image": "ytune",
                                    "small_text": "ytune"
                                },
                                "details": if clean_title.is_empty() { serde_json::Value::Null } else { clean_title.into() },
                                "state": if clean_artist.is_empty() { serde_json::Value::Null } else { format!("by {}", clean_artist).into() },
                                "type": 2,
                                "name": "ytune",
                                "buttons": [
                                    {
                                        "label": "ytune",
                                        "url": "https://github.com/yctwhy/ytune"
                                    }
                                ]
                            });

                            let activity_data_str = serde_json::to_string(&activity_payload)
                                .unwrap_or_else(|e| {
                                    eprintln!("ERROR: Failed to serialize activity: {}", e);
                                    String::new() 
                                });

                            if activity_data_str.is_empty() {
                                return; 
                            }

                            let mut connection_guard = conn_arc_clone_ipc.lock().unwrap();
                            let pid_to_use: u32;
                            if let Some((ref mut file, pid)) = *connection_guard {
                                pid_to_use = pid;
                                match set_activity(file, pid, &activity_data_str) {
                                    Ok(_) => {

                                        match read_message(file) {
                                            Ok((_opcode, response_str)) => {
                                                if response_str.contains("\"cmd\":\"SET_ACTIVITY\"") && response_str.contains("\"evt\":\"ERROR\"") {
                                                    eprintln!("Discord SET_ACTIVITY Error: {}", response_str);
                                                }
                                            },
                                            Err(e) => {
                                                handle_ipc_error(e, Arc::clone(&conn_arc_clone_ipc), CLIENT_ID.to_string(), pid_to_use);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        handle_ipc_error(e, Arc::clone(&conn_arc_clone_ipc), CLIENT_ID.to_string(), pid_to_use);
                                    }
                                }
                            } else {

                                let current_pid = process::id();
                                drop(connection_guard); 

                                let recon_conn_arc = Arc::clone(&conn_arc_clone_ipc);
                                let recon_client_id = CLIENT_ID.to_string();
                                thread::spawn(move || {
                                    attempt_reconnect(recon_conn_arc, recon_client_id, current_pid);
                                });
                            }
                        }
                    }
                }
            }
        })
        .build()?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait; 

        if let wry::application::event::Event::WindowEvent {
            event: wry::application::event::WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit 
        }
    });
}

fn load_window_icon() -> Option<Icon> {
    let icon_bytes = include_bytes!("assets/ytune.png");
    load_from_memory_with_format(icon_bytes, ImageFormat::Png)
        .ok()
        .map(|image| {
            let image = image.into_rgba8();
            let (width, height) = image.dimensions();
            let rgba = image.into_raw();
            Icon::from_rgba(rgba, width, height).ok()
        })
        .flatten()
}

#[cfg(target_os = "windows")]
fn connect_and_handshake(client_id: &str, _pid: u32) -> io::Result<File> {
    connect().and_then(|mut file| {
        send_handshake(&mut file, client_id)?;
        match read_message(&mut file) {
            Ok((1, response_str)) => {
                match serde_json::from_str::<serde_json::Value>(&response_str) {
                    Ok(json_response) => {
                        if json_response.get("cmd").and_then(|v| v.as_str()) == Some("DISPATCH")
                            && json_response.get("evt").and_then(|v| v.as_str()) == Some("READY")
                        {
                            Ok(file)
                        } else {
                            Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Handshake not READY",
                            ))
                        }
                    }
                    Err(e) => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Parse handshake JSON failed: {}", e),
                    )),
                }
            }
            Ok((opcode, _)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unexpected opcode {} in handshake", opcode),
            )),
            Err(e) => Err(e),
        }
    })
}

#[cfg(target_os = "windows")]
fn handle_ipc_error(
    error: io::Error,
    connection_arc: Arc<Mutex<DiscordConnectionState>>,
    client_id: String,
    pid: u32,
) {
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof
    ) {
        eprintln!("Discord pipe broken. Clearing state and attempting reconnect...");
        {
            let mut guard = connection_arc.lock().unwrap();
            *guard = None;
        }

        thread::spawn(move || {
            thread::sleep(Duration::from_secs(2));
            attempt_reconnect(connection_arc, client_id, pid);
        });
    }
}

#[cfg(target_os = "windows")]
fn attempt_reconnect(
    connection_arc: Arc<Mutex<DiscordConnectionState>>,
    client_id: String,
    pid: u32,
) {
    match connect_and_handshake(&client_id, pid) {
        Ok(new_file) => {
            let mut guard = connection_arc.lock().unwrap();
            *guard = Some((new_file, pid));
        }
        Err(e) => {
            eprintln!("Discord reconnection attempt failed: {:?}", e);
            let mut guard = connection_arc.lock().unwrap();
            if guard.is_some() {
                *guard = None;
            }
        }
    }
}