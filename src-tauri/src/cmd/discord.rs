//! Discord RPC Tauri commands
//! 




use crate::config::Config;
use crate::core::discord_rpc;
use crate::process::AsyncHandler;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use tauri::async_runtime::JoinHandle;
use serde::Deserialize;
use futures::StreamExt;
use crate::utils::dirs::app_home_dir;
use std::fs;
use std::time::SystemTime;

static TRAFFIC_UP: AtomicU64 = AtomicU64::new(0);
static TRAFFIC_DOWN: AtomicU64 = AtomicU64::new(0);

// Persistence State
struct TrafficState {
    total_up: u64,
    total_down: u64,
    last_session_up: u64,
    last_session_down: u64,
    last_save_time: SystemTime,
}

static TRAFFIC_STATE: once_cell::sync::Lazy<Mutex<TrafficState>> = once_cell::sync::Lazy::new(|| {
    let mut state = TrafficState {
        total_up: 0,
        total_down: 0,
        last_session_up: 0,
        last_session_down: 0,
        last_save_time: SystemTime::now(),
    };
    // Try calculate path and load
    if let Ok(dir) = app_home_dir() {
        let path = dir.join("traffic_data.json");
        if path.exists() {
             if let Ok(content) = fs::read_to_string(path) {
                 if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                     state.total_up = json["up"].as_u64().unwrap_or(0);
                     state.total_down = json["down"].as_u64().unwrap_or(0);
                 }
             }
        }
    }
    Mutex::new(state)
});

fn save_traffic_data(up: u64, down: u64) {
    if let Ok(dir) = app_home_dir() {
         let path = dir.join("traffic_data.json");
         let json = serde_json::json!({
             "up": up,
             "down": down
         });
         let _ = fs::write(path, json.to_string());
    }
}


static DISCORD_LOOP_HANDLE: once_cell::sync::Lazy<Arc<Mutex<Option<JoinHandle<()>>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

#[derive(Deserialize)]
struct TrafficData {
    up: u64,
    down: u64,
}

/// Helper to format bytes in a human-readable way
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Helper to format speed in a human-readable way
fn format_speed(speed: u64) -> String {
    if speed < 1024 {
        format!("{} B/s", speed)
    } else if speed < 1024 * 1024 {
        format!("{:.1} KB/s", speed as f64 / 1024.0)
    } else {
        format!("{:.1} MB/s", speed as f64 / (1024.0 * 1024.0))
    }
}

/// Toggle Discord Rich Presence on or off
#[tauri::command]
pub async fn toggle_discord_rpc(enabled: bool) -> Result<(), String> {
    if enabled {
        // Get custom app ID from config if set
        let verge = Config::verge().await;
        let verge_data = verge.data_arc();
        let app_id_owned = verge_data.discord_app_id.clone();
        let app_id = app_id_owned.as_ref().map(|s| s.as_str());
        
        discord_rpc::init_discord_rpc(app_id);
        discord_rpc::connect_discord_rpc();
        
        // Small delay to allow connection before updating activity
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        
        // Start the background update loop
        start_discord_update_loop().await;
        
        update_discord_activity().await;
    } else {
        stop_discord_update_loop().await;
        discord_rpc::shutdown_discord_rpc();
    }
    Ok(())
}

/// Start the background loop for periodic Discord updates
async fn start_discord_update_loop() {
    let mut handle_guard = DISCORD_LOOP_HANDLE.lock().await;
    
    // Stop existing loop if any
    if let Some(handle) = handle_guard.take() {
        handle.abort();
    }
    
    let loop_handle = AsyncHandler::spawn(|| async move {
        // Traffic monitor task
        let traffic_monitor = AsyncHandler::spawn(|| async move {
            loop {
                let clash_info = Config::clash().await.data_arc().get_client_info();
                let server = clash_info.server;
                let secret = clash_info.secret.unwrap_or_default();
                let url = format!("http://{}/traffic", server);
                
                let client = reqwest::Client::new();
                let request = client.get(&url);
                let request = if !secret.is_empty() {
                    request.header("Authorization", format!("Bearer {}", secret))
                } else {
                    request
                };

                match request.send().await {
                    Ok(resp) => {
                        let mut stream = resp.bytes_stream();
                        while let Some(item) = stream.next().await {
                            match item {
                                Ok(bytes) => {
                                    if let Ok(data) = serde_json::from_slice::<TrafficData>(&bytes) {
                                        TRAFFIC_UP.store(data.up, Ordering::Relaxed);
                                        TRAFFIC_DOWN.store(data.down, Ordering::Relaxed);
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    Err(_) => {
                        // Reset traffic on error
                        TRAFFIC_UP.store(0, Ordering::Relaxed);
                        TRAFFIC_DOWN.store(0, Ordering::Relaxed);
                    }
                }
                // Wait before reconnecting
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        });

        // Periodic update loop
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            
            // Re-check if still enabled
            let verge_data = Config::verge().await.data_arc();
            if !verge_data.enable_discord_rpc.unwrap_or(false) {
                traffic_monitor.abort();
                break;
            }
            
            update_discord_activity().await;
        }
    });
    
    *handle_guard = Some(loop_handle);
}

/// Stop the background loop
async fn stop_discord_update_loop() {
    let mut handle_guard = DISCORD_LOOP_HANDLE.lock().await;
    if let Some(handle) = handle_guard.take() {
        handle.abort();
    }
    // Also reset traffic data
    TRAFFIC_UP.store(0, Ordering::Relaxed);
    TRAFFIC_DOWN.store(0, Ordering::Relaxed);
}

/// Manually refresh Discord activity (also used internally when proxy mode changes)
#[tauri::command]
pub async fn refresh_discord_activity() -> Result<(), String> {
    update_discord_activity().await;
    Ok(())
}

/// Public function to update Discord activity with current proxy status
/// Called from feat/config.rs when TUN mode or system proxy changes
pub async fn update_discord_activity() {
    let verge = Config::verge().await;
    let verge_data = verge.data_arc();

    // Check if Discord RPC is enabled
    if !verge_data.enable_discord_rpc.unwrap_or(false) {
        return;
    }

    // Determine connection mode
    // Determine connection mode (not displayed anymore, kept if needed for logic but we can remove it)
    // let conn_mode = ... 


    // Traffic info
    let up = TRAFFIC_UP.load(Ordering::Relaxed);
    let down = TRAFFIC_DOWN.load(Ordering::Relaxed);
    
    // Get total traffic info and proxies
    // Get total traffic info and proxies
    let mihomo = crate::core::handle::Handle::mihomo().await;
    // Removed local zero init, we use the persistent state
    
    // Update persistent state
    if let Ok(connections) = mihomo.get_connections().await {
        let current_up = connections.upload_total;
        let current_down = connections.download_total;
        
        let mut state = TRAFFIC_STATE.lock().await;
        
        // Calculate delta
        let delta_up = if current_up >= state.last_session_up {
            current_up - state.last_session_up
        } else {
            current_up // Reset detected
        };
        
        let delta_down = if current_down >= state.last_session_down {
            current_down - state.last_session_down
        } else {
            current_down // Reset detected
        };
        
        state.total_up += delta_up;
        state.total_down += delta_down;
        state.last_session_up = current_up;
        state.last_session_down = current_down;
        
        // Save periodically (e.g. every 10 seconds)
        if state.last_save_time.elapsed().map(|d| d.as_secs() > 10).unwrap_or(true) {
            save_traffic_data(state.total_up, state.total_down);
            state.last_save_time = SystemTime::now();
        }
    }
    
    // Read values for display
    let (total_up, total_down) = {
        let state = TRAFFIC_STATE.lock().await;
        (state.total_up, state.total_down)
    };


    // Get current profile name to help identify the main proxy group
    let profiles = Config::profiles().await;
    let profiles_data = profiles.data_arc();
    let current_profile = profiles_data
        .current
        .as_ref()
        .and_then(|uid| {
            profiles_data.items.as_ref().and_then(|items| {
                items
                    .iter()
                    .find(|p| p.uid.as_ref().map(|u| u.as_str()) == Some(uid.as_str()))
                    .and_then(|p| p.name.clone())
            })
        });


    let details = format!("↑ {} • ↓ {}", 
        format_speed(up), 
        format_speed(down)
    );

    // Clash mode (not displayed anymore)
    // let clash = Config::clash().await;
    // ...


    // Fetch the current selected node
    let mut selected_node = String::new();
    let mut total_proxies = 0;
    
    if let Ok(proxies) = mihomo.get_proxies().await {
        // Logic to determine the "Primary" proxy group
        // 1. Try to find a group matching the Profile Name
        // 2. Fallback to "Proxy", "Default", "Select"
        // 3. Use GLOBAL if nothing else matches specific criteria, OR if GLOBAL is manually set to a specific node (not just fallback)
        
        // Try to identify the main group based on profile name
        let mut main_group_name = String::from("GLOBAL");
        
        if let Some(profile_name) = &current_profile {
            // Check if there is a proxy group that contains the profile name (case-insensitive)
            // e.g. Profile "XppaiCyber" -> Group "XppaiCyber"
            for key in proxies.proxies.keys() {
                 if key.to_lowercase().contains(&profile_name.to_lowercase()) {
                     main_group_name = key.clone();
                     break;
                 }
            }
        }
        
        // If we didn't find a profile-based group, and we are in Rule mode (implied by this logic need),
        // we might want to look for common names if GLOBAL is just "DIRECT" or "REJECT" or seemed weird.
        if main_group_name == "GLOBAL" {
             // Heuristic: If there is a group named "Proxy", use it.
             if proxies.proxies.contains_key("Proxy") {
                 main_group_name = String::from("Proxy");
             }
        }

        if let Some(group) = proxies.proxies.get(&main_group_name) {
            if let Some(now) = &group.now {
                // Iterative resolution
                let mut current = now.clone();
                for _ in 0..10 {
                    if let Some(g) = proxies.proxies.get(&current) {
                        if let Some(next) = &g.now {
                             current = next.clone();
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                selected_node = current;
            }
        }

        
        total_proxies = proxies.proxies.len();
    }

    // Active connections (optional, not currently displayed but available)
    // let mut active_connections = 0;
    // if let Ok(connections) = mihomo.get_connections().await {
    //     active_connections = connections.connections.map(|c| c.len()).unwrap_or(0);
    // }

    // Pretty state: "TUN • Rule • Node"
    // State: "All: ↑ 1.2 MB • ↓ 41.7 MB | ProxyName"
    let state = if !selected_node.is_empty() {
        format!("All: ↑ {} • ↓ {} | {}", 
            format_bytes(total_up), 
            format_bytes(total_down), 
            selected_node
        )
    } else {
        format!("All: ↑ {} • ↓ {}", 
            format_bytes(total_up), 
            format_bytes(total_down)
        )
    };

    // Convert total proxies to party info (1 of Total)
    let mut party_size = None;
    let mut party_max = None;

    if total_proxies > 0 {
        party_size = Some(1);
        party_max = Some(total_proxies as i32);
    }

    discord_rpc::update_discord_activity(&details, &state, party_size, party_max);
}

/// Commands to manually unload (stop) Discord RPC
#[tauri::command]
pub async fn unload_discord_rpc() -> Result<(), String> {
    toggle_discord_rpc(false).await?;
    Ok(())
}

/// Command to manually reload Discord RPC (stop -> wait -> start)
#[tauri::command]
pub async fn trigger_discord_rpc_reload() -> Result<(), String> {
    toggle_discord_rpc(false).await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
    toggle_discord_rpc(true).await?;
    Ok(())
}

/// Initialize Discord RPC on app startup if enabled
pub async fn init_discord_rpc_on_startup() {
    let verge = Config::verge().await;
    let verge_data = verge.data_arc();
    
    if verge_data.enable_discord_rpc.unwrap_or(false) {
        let app_id_owned = verge_data.discord_app_id.clone();
        let app_id = app_id_owned.as_ref().map(|s| s.as_str());
        discord_rpc::init_discord_rpc(app_id);
        discord_rpc::connect_discord_rpc();
        
        // Start the background update loop
        start_discord_update_loop().await;
        
        // Small delay to allow connection
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        update_discord_activity().await;
    }
}

/// Shutdown Discord RPC on app exit
pub fn shutdown_discord_rpc_on_exit() {
    // We don't really need to abort the loop here as the app is exiting,
    // but we can for completeness.
    // However, this is NOT async, so we can't easily lock the mutex.
    // The OS will clean up the tasks.
    discord_rpc::shutdown_discord_rpc();
}

