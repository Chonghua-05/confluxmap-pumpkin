//! Declaring the confluxmap companion channels to the client.
//!
//! Registering event handlers is only half of a plugin-message channel. A
//! confluxmap client refuses to send its HELLO unless the **server has already
//! declared the channel to it**: `ClientPlayNetworking.canSend` gates on
//! `ClientPacketListener.hasChannel(id)`, and when that is false the client
//! silently skips the handshake (the failure is logged at debug level only).
//!
//! A Bukkit server announces channels for its plugins automatically. Pumpkin has
//! no equivalent API, so the plugin emits the vanilla packet itself: a
//! `minecraft:register` custom payload whose body is the newline-free,
//! NUL-separated channel list. That is exactly the packet a Paper server sends
//! on the plugin's behalf.
//!
//! There are two channels - `confluxmap:map_sync` for the handshake and policy
//! exchange, and `confluxmap:waypoints_v1` for the shared waypoint directory -
//! and a client may use either on its own. Both are announced together in one
//! payload so that each is known before the client's join callback runs; the
//! body carries every name with a trailing NUL, which is what [`register_body`]
//! already produces for a list.
//!
//! This is why the channels must be declared **on login/join**, not lazily in
//! reply to something: the client sends HELLO from its own join callback, so a
//! declaration that arrives afterwards is too late for that session.

use pumpkin_plugin_api::Player;

use crate::protocol;

/// The vanilla control channel used to announce plugin-message channels.
pub const REGISTER_CHANNEL: &str = "minecraft:register";

/// The companion channels this plugin announces to every joining client.
///
/// Kept in one place so the login/join declarer, the tests and any future
/// introspection all agree on the set and its order. The order is part of the
/// payload and therefore stable: `confluxmap:map_sync` first, because it is the
/// channel the handshake depends on, then `confluxmap:waypoints_v1`.
pub fn declared_channels() -> Vec<&'static str> {
    vec![protocol::CHANNEL_ID, crate::waypoints::CHANNEL_ID]
}

/// Body of a `minecraft:register` payload for `channels`.
///
/// Names are NUL-separated with a trailing separator, matching what
/// `ClientPacketListener.handleCustomPayload` splits on.
pub fn register_body(channels: &[&str]) -> Vec<u8> {
    let mut body = Vec::new();
    for channel in channels {
        body.extend_from_slice(channel.as_bytes());
        body.push(0);
    }
    body
}

/// Announces the companion channels to `player`.
///
/// Idempotent from the client's point of view: the client keeps a set, so a
/// repeated announcement is a no-op. Returns `false` when the player is a
/// Bedrock client with no Java connection to send on.
pub fn declare_companion_channel(player: &Player) -> bool {
    let Some(java) = player.as_java() else {
        return false;
    };
    let body = register_body(&declared_channels());
    java.send_custom_payload(REGISTER_CHANNEL, &body);
    true
}

/// Which side of the join sequence a declaration happened on, for logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Fired before the client's own join callback - the only useful stage.
    Login,
    /// Fired around the join; kept as a second attempt.
    Join,
    /// Fired in reply to a late channel registration from the client.
    Late,
}

impl Stage {
    /// A short tag for log lines.
    pub fn tag(self) -> &'static str {
        match self {
            Stage::Login => "login",
            Stage::Join => "join",
            Stage::Late => "late",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_body_is_nul_separated_with_a_trailing_separator() {
        assert_eq!(
            register_body(&["confluxmap:map_sync"]),
            b"confluxmap:map_sync\0"
        );
    }

    #[test]
    fn register_body_handles_several_channels() {
        assert_eq!(register_body(&["a", "b"]), b"a\0b\0");
    }

    #[test]
    fn the_companion_channel_name_is_the_protocol_one() {
        assert_eq!(protocol::CHANNEL_ID, "confluxmap:map_sync");
        let body = register_body(&[protocol::CHANNEL_ID]);
        assert!(body.ends_with(b"\0"));
        assert_eq!(body.len(), protocol::CHANNEL_ID.len() + 1);
    }

    #[test]
    fn the_declared_set_is_the_companion_channels_in_a_stable_order() {
        assert_eq!(
            declared_channels(),
            vec![protocol::CHANNEL_ID, crate::waypoints::CHANNEL_ID]
        );
    }

    #[test]
    fn both_channels_travel_in_one_payload_each_nul_terminated() {
        let channels = declared_channels();
        let body = register_body(&channels);

        // Rebuild the expected body from the set: every name followed by a NUL,
        // including the last one, so a client splitting on NUL sees exactly the
        // two channels and no trailing empty entry.
        let expected: Vec<u8> = channels
            .iter()
            .flat_map(|channel| channel.as_bytes().iter().copied().chain(std::iter::once(0)))
            .collect();
        assert_eq!(body, expected);
        assert_eq!(
            body.iter().filter(|&&byte| byte == 0).count(),
            channels.len()
        );
        assert!(body.ends_with(b"\0"));
    }

    #[test]
    fn the_two_companion_channels_are_distinct() {
        assert_ne!(protocol::CHANNEL_ID, crate::waypoints::CHANNEL_ID);
    }
}
