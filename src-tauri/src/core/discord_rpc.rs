//! Discord Rich Presence integration for Clash Verge Rev
//!
//! This module provides Discord Rich Presence functionality, allowing users
//! to display their Clash connection status on their Discord profile.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use log::{debug, error, info, warn};
use parking_lot::Mutex;
use tokio::sync::mpsc;

/// Default Discord Application ID for Clash Verge Rev
/// Users can override this with their own Application ID
const DEFAULT_APP_ID: &str = "1057691699440259096";

/// Commands that can be sent to the Discord RPC worker thread
#[derive(Debug)]
#[allow(dead_code)]
pub enum RpcCommand {
    Connect,
    Disconnect,
    UpdateActivity {
        details: String,
        state: String,
        party_size: Option<i32>,
        party_max: Option<i32>,
    },
    ClearActivity,
    Shutdown,
}

/// Manages the Discord Rich Presence connection
pub struct DiscordRpcManager {
    sender: Option<mpsc::UnboundedSender<RpcCommand>>,
    connected: Arc<Mutex<bool>>,
    start_time: Arc<Mutex<Option<i64>>>,
}

impl Default for DiscordRpcManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DiscordRpcManager {
    /// Create a new Discord RPC manager
    pub fn new() -> Self {
        Self {
            sender: None,
            connected: Arc::new(Mutex::new(false)),
            start_time: Arc::new(Mutex::new(None)),
        }
    }

    /// Initialize and start the Discord RPC worker
    pub fn init(&mut self, app_id: Option<&str>) {
        let app_id = app_id.unwrap_or(DEFAULT_APP_ID).to_string();
        let connected = self.connected.clone();
        let start_time = self.start_time.clone();

        let (tx, mut rx) = mpsc::unbounded_channel::<RpcCommand>();
        self.sender = Some(tx);

        // Spawn worker thread for Discord IPC (blocking operations)
        std::thread::spawn(move || {
            let mut client: Option<DiscordIpcClient> = None;

            while let Some(cmd) = rx.blocking_recv() {
                match cmd {
                    RpcCommand::Connect => {
                        if client.is_some() {
                            debug!("Discord RPC already connected");
                            continue;
                        }

                        match DiscordIpcClient::new(&app_id) {
                            Ok(mut new_client) => {
                                match new_client.connect() {
                                    Ok(_) => {
                                        info!("Discord RPC connected successfully");
                                        *connected.lock() = true;
                                        
                                        // Set start time for elapsed display
                                        let now = SystemTime::now()
                                            .duration_since(UNIX_EPOCH)
                                            .map(|d| d.as_secs() as i64)
                                            .unwrap_or(0);
                                        *start_time.lock() = Some(now);
                                        
                                        client = Some(new_client);
                                    }
                                    Err(e) => {
                                        warn!("Failed to connect to Discord: {}", e);
                                        *connected.lock() = false;
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to create Discord IPC client: {}", e);
                            }
                        }
                    }

                    RpcCommand::Disconnect => {
                        if let Some(ref mut c) = client {
                            if let Err(e) = c.close() {
                                warn!("Error closing Discord connection: {}", e);
                            }
                        }
                        client = None;
                        *connected.lock() = false;
                        *start_time.lock() = None;
                        info!("Discord RPC disconnected");
                    }

                    RpcCommand::UpdateActivity { details, state, party_size, party_max } => {
                        if let Some(ref mut c) = client {
                            let timestamp = *start_time.lock();
                            
                            let mut act = activity::Activity::new()
                                .details(&details)
                                .state(&state)
                                .assets(
                                    activity::Assets::new()
                                        .large_image("clash_verge")
                                        .large_text("Clash Verge Rev"),
                                );

                            if let Some(ts) = timestamp {
                                act = act.timestamps(
                                    activity::Timestamps::new().start(ts),
                                );
                            }

                            if let (Some(size), Some(max)) = (party_size, party_max) {
                                act = act.party(activity::Party::new().size([size, max]));
                            }

                            if let Err(e) = c.set_activity(act) {
                                warn!("Failed to update Discord activity: {}", e);
                                // Try to reconnect on next update
                                *connected.lock() = false;
                            }
                        }
                    }

                    RpcCommand::ClearActivity => {
                        if let Some(ref mut c) = client {
                            if let Err(e) = c.clear_activity() {
                                warn!("Failed to clear Discord activity: {}", e);
                            }
                        }
                    }

                    RpcCommand::Shutdown => {
                        if let Some(ref mut c) = client {
                            let _ = c.clear_activity();
                            let _ = c.close();
                        }
                        info!("Discord RPC worker shutting down");
                        break;
                    }
                }
            }
        });
    }

    /// Connect to Discord
    pub fn connect(&self) {
        if let Some(ref tx) = self.sender {
            let _ = tx.send(RpcCommand::Connect);
        }
    }

    /// Disconnect from Discord
    pub fn disconnect(&self) {
        if let Some(ref tx) = self.sender {
            let _ = tx.send(RpcCommand::Disconnect);
        }
    }

    /// Update the Discord activity
    pub fn update_activity(&self, details: impl Into<String>, state: impl Into<String>, party_size: Option<i32>, party_max: Option<i32>) {
        if let Some(ref tx) = self.sender {
            let _ = tx.send(RpcCommand::UpdateActivity {
                details: details.into(),
                state: state.into(),
                party_size,
                party_max,
            });
        }
    }

    /// Clear the current activity
    #[allow(dead_code)]
    pub fn clear_activity(&self) {
        if let Some(ref tx) = self.sender {
            let _ = tx.send(RpcCommand::ClearActivity);
        }
    }

    /// Shutdown the RPC worker
    pub fn shutdown(&self) {
        if let Some(ref tx) = self.sender {
            let _ = tx.send(RpcCommand::Shutdown);
        }
    }

    /// Check if connected to Discord
    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        *self.connected.lock()
    }
}

impl Drop for DiscordRpcManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Global Discord RPC manager instance
static DISCORD_RPC: once_cell::sync::Lazy<Mutex<Option<DiscordRpcManager>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

/// Initialize the global Discord RPC manager
pub fn init_discord_rpc(app_id: Option<&str>) {
    let mut manager = DiscordRpcManager::new();
    manager.init(app_id);
    *DISCORD_RPC.lock() = Some(manager);
}

/// Connect to Discord RPC
pub fn connect_discord_rpc() {
    let guard = DISCORD_RPC.lock();
    if let Some(ref manager) = *guard {
        manager.connect();
    }
}

/// Disconnect from Discord RPC
#[allow(dead_code)]
pub fn disconnect_discord_rpc() {
    let guard = DISCORD_RPC.lock();
    if let Some(ref manager) = *guard {
        manager.disconnect();
    }
}

/// Update Discord RPC activity with current proxy status
pub fn update_discord_activity(details: &str, state: &str, party_size: Option<i32>, party_max: Option<i32>) {
    let guard = DISCORD_RPC.lock();
    if let Some(ref manager) = *guard {
        manager.update_activity(details, state, party_size, party_max);
    }
}

/// Shutdown Discord RPC
pub fn shutdown_discord_rpc() {
    {
        let guard = DISCORD_RPC.lock();
        if let Some(ref manager) = *guard {
            manager.shutdown();
        }
    }
    *DISCORD_RPC.lock() = None;
}
