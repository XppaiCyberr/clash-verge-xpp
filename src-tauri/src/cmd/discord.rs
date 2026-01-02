//! Discord RPC Tauri commands

use crate::config::Config;
use crate::core::discord_rpc;
use crate::process::AsyncHandler;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use tauri::async_runtime::JoinHandle;
use serde::Deserialize;
use futures::StreamExt;

static TRAFFIC_UP: AtomicU64 = AtomicU64::new(0);
static TRAFFIC_DOWN: AtomicU64 = AtomicU64::new(0);

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
    let conn_mode = if verge_data.enable_tun_mode.unwrap_or(false) {
        "TUN"
    } else if verge_data.enable_system_proxy.unwrap_or(false) {
        "System Proxy"
    } else {
        "Clash Active"
    };

    // Traffic info
    let up = TRAFFIC_UP.load(Ordering::Relaxed);
    let down = TRAFFIC_DOWN.load(Ordering::Relaxed);
    
    // Get total traffic info and proxies
    let mihomo = crate::core::handle::Handle::mihomo().await;
    let mut total_up = 0;
    let mut total_down = 0;
    
    if let Ok(connections) = mihomo.get_connections().await {
        total_up = connections.upload_total;
        total_down = connections.download_total;
    }

    // Get current profile name and clash mode
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
        })
        .unwrap_or_else(|| "No Profile".into());

    let details = format!("↑ {} • ↓ {} (All: ↑ {} • ↓ {}) | {}", 
        format_speed(up), 
        format_speed(down), 
        format_bytes(total_up), 
        format_bytes(total_down),
        current_profile
    );

    let clash = Config::clash().await;
    let clash_data = clash.data_arc();
    let clash_mode = clash_data
        .0
        .get("mode")
        .and_then(|v| v.as_str())
        .map(|s| {
            let mut c = s.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .unwrap_or_else(|| "Rule".to_string());

    // Fetch the current selected node
    let mut selected_node = String::new();
    let mut total_proxies = 0;
    
    if let Ok(proxies) = mihomo.get_proxies().await {
        if let Some(global) = proxies.proxies.get("GLOBAL") {
            if let Some(now) = &global.now {
                selected_node = now.clone();
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
    let state = if !selected_node.is_empty() {
        format!("{} • {} • {}", conn_mode, clash_mode, selected_node)
    } else {
        format!("{} • {}", conn_mode, clash_mode)
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

