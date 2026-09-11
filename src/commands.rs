//! The `/cfm` command tree: operator-facing introspection.
//!
//! All four subcommands are read-mostly. `/cfm reload` re-reads the process
//! environment, which is worth having but rarely changes anything: the host
//! injects `CFM_*` once, at server start, so editing them in `pumpkin.toml`
//! normally requires a restart. The command reports that rather than pretending
//! otherwise.

use std::fmt::Write as _;

use pumpkin_plugin_api::{
    Result, Server,
    command::{CommandError, CommandSender, ConsumedArgs},
    commands::CommandHandler,
    text::TextComponent,
};

use crate::config::Config;
use crate::state;

/// `/cfm status` - configuration, counters and the running server version.
pub struct StatusHandler;

impl CommandHandler for StatusHandler {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let config = state::config_snapshot();
        let counters = state::counters();
        let server_version = state::server_version(&server);

        let mut out = String::from("== confluxmap (Pumpkin) ==\n");
        out.push_str(&config.describe(server_version.as_deref()));
        let _ = writeln!(
            out,
            "handshakes     = {} hellos, {} policies sent, {} send failures, {} channels registered",
            counters.hellos,
            counters.policies_sent,
            counters.send_failures,
            counters.channels_registered,
        );
        out.push_str(&format!("mspt           = {:.2}\n", server.get_mspt()));
        reply(&sender, out)
    }
}

/// `/cfm seed` - just the seed-relevant part, for a quick check.
pub struct SeedHandler;

impl CommandHandler for SeedHandler {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let config = state::config_snapshot();
        let server_version = state::server_version(&server);

        let mut out = String::from("== confluxmap seed ==\n");
        match config.seed {
            Some(seed) => {
                let _ = writeln!(out, "CFM_SEED        = {seed}");
                let _ = writeln!(out, "world_id        = {}", config.world_id());
                let _ = writeln!(
                    out,
                    "worldgen        = {}",
                    config.resolve_worldgen(server_version.as_deref())
                );
                if config.grants_seed() {
                    out.push_str("clients receive = seedGranted=1, correctionsEnabled=0\n");
                } else {
                    out.push_str("clients receive = seedGranted=0 (CFM_SHARE_SEED is false)\n");
                }
            }
            None => {
                let _ = writeln!(
                    out,
                    "CFM_SEED is unset. Set it in [plugins.overrides.confluxmap-pumpkin.environment] \
                     in pumpkin.toml, then restart the server."
                );
            }
        }
        reply(&sender, out)
    }
}

/// `/cfm hello` - what the last handshakes looked like, and the last frame's bytes.
pub struct HelloHandler;

impl CommandHandler for HelloHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let mut out = String::from("== confluxmap handshakes ==\n");

        let recent = state::recent_handshakes();
        if recent.is_empty() {
            out.push_str("none yet. A confluxmap client must connect and send HELLO.\n");
        } else {
            let _ = writeln!(out, "recent ({}):", recent.len());
            for line in recent {
                let _ = writeln!(out, "  {line}");
            }
        }

        match state::last_policy() {
            Some(bytes) => {
                let _ = writeln!(
                    out,
                    "last HELLO_POLICY = {} bytes\n  hex = {}",
                    bytes.len(),
                    state::hex(&bytes)
                );
            }
            None => out.push_str("last HELLO_POLICY = <none sent yet>\n"),
        }

        // Which channels clients actually registered. When a client that should
        // have the mod never sends a HELLO, this is the first thing to look at:
        // an empty list means the connection never got modded, a list without
        // `confluxmap:map_sync` means a modded-but-wrong client (e.g. a
        // Forge-family loader, which cannot load a Fabric mod at all).
        let channels = state::recent_channels();
        if channels.is_empty() {
            out.push_str("client channels = <none registered>\n");
        } else {
            let _ = writeln!(out, "client channels ({}):", channels.len());
            for channel in &channels {
                let _ = writeln!(out, "  {channel}");
            }
            if !channels.iter().any(|c| c == crate::protocol::CHANNEL_ID) {
                out.push_str(
                    "NOTE: none of these is the companion channel, so no HELLO can arrive.\n",
                );
            }
        }
        reply(&sender, out)
    }
}

/// `/cfm reload` - re-read the environment and reinstall the configuration.
pub struct ReloadHandler;

impl CommandHandler for ReloadHandler {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let previous = state::config_snapshot();
        let next = Config::from_env();
        let server_version = state::server_version(&server);

        let mut out = String::from("== confluxmap reload ==\n");
        if next == previous {
            out.push_str("no change (the environment is fixed at server start)\n");
        } else {
            out.push_str("configuration changed:\n");
            let _ = writeln!(
                out,
                "- before:\n{}",
                previous.describe(server_version.as_deref())
            );
            let _ = writeln!(
                out,
                "+ after:\n{}",
                next.describe(server_version.as_deref())
            );
        }
        state::set_config(next);
        reply(&sender, out)
    }
}

/// Sends a multi-line block of text back to the invoker.
fn reply(sender: &CommandSender, body: String) -> Result<i32, CommandError> {
    sender.send_message(TextComponent::text(&body));
    Ok(0)
}
