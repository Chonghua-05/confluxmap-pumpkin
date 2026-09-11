//! The `/cfm` command tree: operator-facing introspection.
//!
//! Most subcommands are read-mostly. `/cfm reload` re-reads `config.toml`,
//! which is the point of keeping the configuration in a file: an edit takes
//! effect without a restart. Clients already in the world keep the policy they
//! were sent, since a confluxmap client handshakes once per session; players who
//! join after the reload get the new one.
//!
//! The `waypoints` family exposes the shared directory: `waypoints` prints its
//! state, `waypoints list [page]` pages through it, and `waypoints clear` is the
//! only mutating subcommand. The handlers only format what the `waypoints`
//! module reports, so storage, limits and persistence stay in one place.

use std::fmt::Write as _;

use pumpkin_plugin_api::{
    Result, Server,
    command::{Arg, CommandError, CommandSender, ConsumedArgs},
    command_wit::Number,
    commands::CommandHandler,
    text::TextComponent,
};

use crate::config;
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
                let _ = writeln!(out, "seed            = {seed}");
                let _ = writeln!(out, "world_id        = {}", config.world_id());
                let _ = writeln!(
                    out,
                    "worldgen        = {}",
                    config.resolve_worldgen(server_version.as_deref())
                );
                if config.grants_seed() {
                    out.push_str("clients receive = seedGranted=1, correctionsEnabled=0\n");
                } else {
                    out.push_str("clients receive = seedGranted=0 (share_seed = false)\n");
                }
            }
            None => {
                let _ = writeln!(
                    out,
                    "no seed configured. Fill in `seed` in {}, then run `/cfm reload`.",
                    config::operator_path()
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

/// `/cfm reload` - re-read `config.toml` and reinstall the configuration.
pub struct ReloadHandler;

impl CommandHandler for ReloadHandler {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let previous = state::config_snapshot();
        let server_version = state::server_version(&server);

        let mut out = String::from("== confluxmap reload ==\n");
        let Some(folder) = state::data_folder() else {
            out.push_str("the plugin has not been loaded yet; nothing to reload\n");
            return reply(&sender, out);
        };

        let loaded = config::load(folder);
        let next = loaded.config;
        for note in &loaded.warnings {
            let _ = writeln!(out, "warning: {note}");
        }
        if next == previous {
            out.push_str("no change\n");
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
        out.push_str("players already in the world keep the policy they were sent.\n");
        reply(&sender, out)
    }
}

/// Name of the optional `page` argument on `/cfm waypoints list`.
///
/// Exposed so the command tree in `lib.rs` can register the argument under the
/// same name this handler looks it up by; the two must not drift apart.
pub const PAGE_ARGUMENT: &str = "page";

/// `/cfm waypoints` - the state of the shared waypoint directory.
pub struct WaypointsHandler;

impl CommandHandler for WaypointsHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let mut out = String::from("== confluxmap waypoints ==\n");
        out.push_str(&crate::waypoints::describe());
        reply(&sender, out)
    }
}

/// `/cfm waypoints list [page]` - one page of the directory.
pub struct WaypointsListHandler;

impl CommandHandler for WaypointsListHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let page = page_from_args(&args);
        let mut out = String::from("== confluxmap waypoints list ==\n");
        out.push_str(&crate::waypoints::list_page(page));
        reply(&sender, out)
    }
}

/// `/cfm waypoints clear` - empty the directory and persist the change.
pub struct WaypointsClearHandler;

impl CommandHandler for WaypointsClearHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let mut out = String::from("== confluxmap waypoints clear ==\n");
        match crate::waypoints::clear() {
            Ok(removed) => {
                let _ = writeln!(out, "removed {removed} waypoint(s); the directory is now empty.");
            }
            // A persistence failure is not a usage error, so it is reported in
            // the text and still exits `Ok(0)`: the operator needs the reason,
            // the server does not need a failed command.
            Err(reason) => {
                let _ = writeln!(out, "could not clear the directory: {reason}");
            }
        }
        reply(&sender, out)
    }
}

/// Reads the page number out of a parsed [`Arg`].
///
/// An argument registered as `ArgumentType::Integer` arrives as [`Arg::Num`].
/// Anything else is not a usable page and falls back to page 1 rather than
/// panicking: a missing key (the host reports the empty string as
/// [`Arg::Simple`]), a bound error, or a non-integer value.
fn page_from_arg(arg: &Arg) -> u32 {
    let Arg::Num(Ok(value)) = arg else {
        return 1;
    };
    match value {
        Number::Int32(page) if *page >= 1 => *page as u32,
        Number::Int64(page) if *page >= 1 => u32::try_from(*page).unwrap_or(1),
        _ => 1,
    }
}

/// Reads the optional `page` argument, defaulting to page 1 when absent.
fn page_from_args(args: &ConsumedArgs) -> u32 {
    page_from_arg(&args.get_value(PAGE_ARGUMENT))
}

/// Sends a multi-line block of text back to the invoker.
fn reply(sender: &CommandSender, body: String) -> Result<i32, CommandError> {
    sender.send_message(TextComponent::text(&body));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_plugin_api::command_wit::NotInBounds;

    #[test]
    fn a_positive_integer_is_used_as_the_page() {
        assert_eq!(page_from_arg(&Arg::Num(Ok(Number::Int32(3)))), 3);
        assert_eq!(page_from_arg(&Arg::Num(Ok(Number::Int64(7)))), 7);
    }

    #[test]
    fn a_missing_or_unusable_argument_defaults_to_page_one() {
        // The host reports an absent key as an empty string, not as an error.
        assert_eq!(page_from_arg(&Arg::Simple(String::new())), 1);
        // Zero and negative pages do not exist.
        assert_eq!(page_from_arg(&Arg::Num(Ok(Number::Int32(0)))), 1);
        assert_eq!(page_from_arg(&Arg::Num(Ok(Number::Int32(-4)))), 1);
        assert_eq!(page_from_arg(&Arg::Num(Ok(Number::Int64(-1)))), 1);
        // A non-integer argument is not a page either.
        assert_eq!(page_from_arg(&Arg::Num(Ok(Number::Float64(2.0)))), 1);
        // A bound error is carried inside `Arg::Num`, so it must not slip
        // through to a panic.
        assert_eq!(
            page_from_arg(&Arg::Num(Err(NotInBounds::LowerBound((
                Number::Int32(1),
                Number::Int32(9)
            ))))),
            1
        );
    }
}
