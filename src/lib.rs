//! ConfluxMap companion for the [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin)
//! Minecraft server.
//!
//! ConfluxMap predicts terrain from the world seed, so a multiplayer client needs
//! two things from the server: the **seed** and the **worldgen version**. This
//! plugin supplies exactly those and nothing else.
//!
//! On every `confluxmap:map_sync` HELLO it answers with a `HELLO_POLICY` that
//! grants the seed and declares corrections disabled. The client then renders the
//! predicted underlay locally and never asks for authoritative patches - which is
//! what makes this plugin small enough to survive Pumpkin's API churn: it touches
//! only plugin messaging, one event, a version string, and its own config file.
//!
//! The seed cannot be discovered (see [`config`]), so it is read from the
//! plugin's data folder; that is the one directory the WASI sandbox opens.
//!
//! Because only a client with confluxmap installed registers that channel and
//! sends a HELLO, the "modded clients only" requirement needs no player filtering.
//!
//! The plugin also **announces the channel to the client** on login/join (see
//! [`channel`]). That is not optional: a confluxmap client gates its HELLO on the
//! server having declared the channel first, which a Bukkit server does
//! automatically and Pumpkin does not.

mod channel;
mod commands;
mod config;
mod handshake;
mod protocol;
mod state;

use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata, Result, Server,
    command::Command,
    command::CommandNode,
    events::{
        EventData, EventHandler, EventPriority, PlayerCustomPayloadEvent, PlayerJoinEvent,
        PlayerLoginEvent, PlayerRegisterChannelEvent,
    },
    permissions, register_plugin,
};
use tracing::{debug, info, warn};

use crate::config::PLUGIN_NAME;

/// Declares the companion channel to a client **before** it runs its own join
/// callback.
///
/// This is the load-bearing handler in the whole plugin. A confluxmap client
/// only sends HELLO if the server has already announced the channel
/// (`ClientPacketListener.hasChannel`); a Paper server does that for its plugins
/// automatically, Pumpkin does not. Skipping this makes the plugin look correct
/// in every log line while no client ever speaks.
struct DeclareOnLoginHandler;

impl EventHandler<PlayerLoginEvent> for DeclareOnLoginHandler {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerLoginEvent>,
    ) -> EventData<PlayerLoginEvent> {
        let name = event.player.get_name();
        // A returning session starts with a clean late-announcement claim.
        state::reset_late_announce(&name);
        if channel::declare_companion_channel(&event.player) {
            info!(
                "[confluxmap] announced {} to {name} at {}",
                protocol::CHANNEL_ID,
                channel::Stage::Login.tag()
            );
        }
        event
    }
}

/// Second attempt at the announcement, for a client whose join callback outran
/// the login event. Harmless when the first one already landed.
struct DeclareOnJoinHandler;

impl EventHandler<PlayerJoinEvent> for DeclareOnJoinHandler {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerJoinEvent>,
    ) -> EventData<PlayerJoinEvent> {
        if channel::declare_companion_channel(&event.player) {
            info!(
                "[confluxmap] re-announced {} to {} at {}",
                protocol::CHANNEL_ID,
                event.player.get_name(),
                channel::Stage::Join.tag()
            );
        }
        event
    }
}

/// Logs each join with the client's edition/brand, so an operator can tell at a
/// glance whether a player is running a confluxmap-capable client.
///
/// The brand is logged because it is the first thing to check when a client with
/// the mod never registers the companion channel (wrong loader build, or the mod
/// failing to initialise on that side).
struct JoinHandler;

impl EventHandler<PlayerJoinEvent> for JoinHandler {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerJoinEvent>,
    ) -> EventData<PlayerJoinEvent> {
        let name = event.player.get_name();
        let client = match event.player.as_java() {
            Some(java) => format!("{:?} brand={:?}", java.get_version(), java.get_brand()),
            None => "bedrock".to_string(),
        };
        info!("[confluxmap] join: {name} ({client})");
        event
    }
}

/// Runs the handshake whenever a client speaks on `confluxmap:map_sync`.
struct PayloadHandler;

impl EventHandler<PlayerCustomPayloadEvent> for PayloadHandler {
    fn handle(
        &self,
        server: Server,
        event: EventData<PlayerCustomPayloadEvent>,
    ) -> EventData<PlayerCustomPayloadEvent> {
        // The channel carries other message types too; anything that is not a
        // HELLO belongs to a fuller companion implementation and is ignored.
        if event.channel != protocol::CHANNEL_ID {
            return event;
        }

        let version = state::server_version(&server);
        if let handshake::Outcome::NotHello { type_byte } =
            handshake::handle_payload(&event.player, &event.data, version.as_deref())
        {
            info!(
                "[confluxmap] ignored {} byte payload on {} from {} (type byte {type_byte:?})",
                event.data.len(),
                protocol::CHANNEL_ID,
                event.player.get_name()
            );
        }
        event
    }
}

/// Records which clients actually registered the companion channel.
///
/// *Every* registration is recorded, not just ours. When a client that should
/// have the mod never sends a HELLO, the useful question is "what did it
/// register instead?" - and the answer is only visible if we keep the full list
/// rather than just the one channel we care about. Foreign channels are kept at
/// debug level (and in `/cfm status`) so a chatty loader cannot flood the log.
///
/// This is also the last place a channel announcement can still help: a modded
/// client whose join callback outran both earlier announcements is re-announced
/// here, at most once per session.
struct RegisterChannelHandler;

impl EventHandler<PlayerRegisterChannelEvent> for RegisterChannelHandler {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerRegisterChannelEvent>,
    ) -> EventData<PlayerRegisterChannelEvent> {
        let name = event.player.get_name();
        state::note_channel(&event.channel);
        if event.channel == protocol::CHANNEL_ID {
            state::count_channel_registered();
            info!(
                "[confluxmap] {name} registered {} - confluxmap client detected",
                protocol::CHANNEL_ID
            );
        } else {
            // Per-channel detail is debug-level only. A modded client registers
            // dozens of channels in one burst; at info level that buries every
            // other line the plugin emits. `/cfm status` still lists them.
            debug!("[confluxmap] {name} registered {}", event.channel);
            // A client that registers other channels is a modded client that may
            // not have seen our announcement in time. Re-announce once per
            // session; if its loader retries the handshake later, it will now
            // succeed.
            if state::claim_late_announce(&name)
                && channel::declare_companion_channel(&event.player)
            {
                info!(
                    "[confluxmap] re-announced {} to {name} at {}",
                    protocol::CHANNEL_ID,
                    channel::Stage::Late.tag()
                );
            }
        }
        event
    }
}

/// The plugin itself.
struct ConfluxMapPlugin;

impl Plugin for ConfluxMapPlugin {
    fn new() -> Self {
        ConfluxMapPlugin
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: PLUGIN_NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["confluxmap".into()],
            description: "Grants confluxmap clients the world seed and worldgen version so they \
                          can render the predicted map. Corrections stay disabled."
                .into(),
            dependencies: vec![],
            permissions: vec![
                permissions::FS_READ_DATA.into(),
                permissions::FS_WRITE_DATA.into(),
            ],
        }
    }

    fn on_load(&self, context: Context) -> Result<()> {
        let data_folder = context.get_data_folder();
        state::set_data_folder(data_folder.clone());
        let loaded = config::load(&data_folder);
        let config = loaded.config;

        info!("=================================================");
        info!(
            "[confluxmap] {} v{} loading",
            PLUGIN_NAME,
            env!("CARGO_PKG_VERSION")
        );
        info!("[confluxmap] channel = {}", protocol::CHANNEL_ID);
        for note in &loaded.warnings {
            warn!("[confluxmap] {note}");
        }
        match config.seed {
            Some(seed) => info!(
                "[confluxmap] seed = {seed}; world_id = {}; worldgen override = {:?}",
                config.world_id(),
                config.worldgen_override
            ),
            None => warn!(
                "[confluxmap] no seed configured, so clients will be told seedGranted=0. \
                 Fill in `seed` in {}, then run `/cfm reload`.",
                config::operator_path()
            ),
        }
        state::set_config(config);

        context
            .register_event_handler(DeclareOnLoginHandler, EventPriority::Highest, false)
            .map_err(|e| format!("login declarer: {e}"))?;
        context
            .register_event_handler(DeclareOnJoinHandler, EventPriority::Normal, false)
            .map_err(|e| format!("join declarer: {e}"))?;
        context
            .register_event_handler(PayloadHandler, EventPriority::Normal, false)
            .map_err(|e| format!("payload handler: {e}"))?;
        context
            .register_event_handler(RegisterChannelHandler, EventPriority::Normal, false)
            .map_err(|e| format!("channel handler: {e}"))?;
        context
            .register_event_handler(JoinHandler, EventPriority::Low, false)
            .map_err(|e| format!("join handler: {e}"))?;
        info!("[confluxmap] event handlers registered");

        let root = Command::new(&["cfm".to_string()], "ConfluxMap companion")
            .then(CommandNode::literal("status").execute(commands::StatusHandler))
            .then(CommandNode::literal("seed").execute(commands::SeedHandler))
            .then(CommandNode::literal("hello").execute(commands::HelloHandler))
            .then(CommandNode::literal("reload").execute(commands::ReloadHandler));
        context.register_command(root, "cfm.use");
        info!("[confluxmap] /cfm registered (status|seed|hello|reload)");
        info!("[confluxmap] load complete");
        info!("=================================================");
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<()> {
        warn!("[confluxmap] unloaded");
        Ok(())
    }
}

// Requests access to the plugin's own data folder, where `config.toml` lives.
//
// The pair is deliberate: read is what the plugin needs on every start, write is
// what lets it drop the annotated template the first time. No other permission is
// requested - not the environment, not the network.
register_plugin!(ConfluxMapPlugin);
