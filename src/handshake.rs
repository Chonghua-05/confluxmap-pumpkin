//! Answering the confluxmap handshake on `confluxmap:map_sync`.
//!
//! A confluxmap client sends `HELLO_C2S` as soon as it is in the world. The
//! server replies with up to three frames, in this order:
//!
//! 1. `0x12 MAP_CAPABILITIES` - only when the client offered capabilities.
//! 2. `0x13 SERVER_INSTANCE` - only when that offer included `SERVER_INSTANCE`.
//! 3. `0x02 HELLO_POLICY` - always, last: the client opens its session on it.
//!
//! The order is not cosmetic. The client refuses a capability-gated message it
//! has not selected, and it resolves the selection out of frame 1; sending the
//! policy first would have it open a session that knows about no capabilities,
//! and frame 2 would then be rejected.
//!
//! Gate-keeping comes for free from the event model. Only a client that has
//! confluxmap installed registers `confluxmap:map_sync` and sends a HELLO, so a
//! vanilla client never produces this event at all - there is no player
//! filtering to do here.

use pumpkin_plugin_api::Player;
use tracing::{info, warn};

use crate::config::Config;
use crate::protocol::{self, HelloC2S, Offer, flags};
use crate::state;

/// What happened to one inbound `confluxmap:map_sync` payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not a HELLO frame; another message type on the same channel, or garbage.
    NotHello {
        /// The leading type byte, for the log.
        type_byte: Option<u8>,
    },
    /// Every frame of the reply was accepted by the transport.
    Replied {
        /// One-line human-readable summary.
        summary: String,
        /// Total bytes handed to the transport.
        bytes: usize,
    },
    /// A frame was built but the transport refused to send it.
    SendFailed {
        /// One-line human-readable summary.
        summary: String,
    },
}

/// The frames one HELLO is answered with, in wire order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// Frames to send, in order.
    pub frames: Vec<Vec<u8>>,
    /// What the client's HELLO advertised.
    pub offer: Offer,
}

/// Builds the reply frames for one decoded HELLO.
///
/// Split out from [`handle_payload`] so the whole exchange can be asserted
/// without a live player. `plugin_version` is this plugin's own version, which
/// the capability envelope reports as the server's mod version.
pub fn build_reply(
    config: &Config,
    server_version: Option<&str>,
    instance_id: &str,
    plugin_version: &str,
    hello: &HelloC2S,
) -> Reply {
    let offer = protocol::parse_offer(&hello.predictor_version);
    let mut frames = Vec::new();

    if offer.caps2 {
        // Only the capabilities this plugin actually serves are granted. A
        // granted capability is a promise to send its messages, and every other
        // one in the enum belongs to a correction stream that does not exist
        // here.
        let capabilities: Vec<(u8, u8)> = if offer.server_instance {
            vec![(
                protocol::CAP_SERVER_INSTANCE,
                protocol::CAP_SERVER_INSTANCE_VERSION,
            )]
        } else {
            Vec::new()
        };
        frames.push(protocol::build_map_capabilities(
            plugin_version,
            &capabilities,
        ));
        if offer.server_instance {
            frames.push(protocol::build_server_instance(instance_id));
        }
    }

    frames.push(build_policy(config, server_version));
    Reply { frames, offer }
}

/// Builds the `HELLO_POLICY_S2C` frame for `config`.
///
/// Split out from [`handle_payload`] so the wire bytes can be produced and
/// asserted without a live player.
pub fn build_policy(config: &Config, server_version: Option<&str>) -> Vec<u8> {
    let flags = if config.grants_seed() {
        flags::SEED_GRANTED
    } else {
        0
    };
    protocol::build_hello_policy(
        flags,
        &config.world_id(),
        &config.resolve_worldgen(server_version),
        config.budgets,
        &config.dims_for_policy(),
    )
}

/// Handles one inbound payload on [`protocol::CHANNEL_ID`].
///
/// `server_version` is the running server's `pumpkin-version`, used to derive
/// the worldgen version the client feeds to cubiomes. `instance_id` is the
/// identity advertised to capability-aware clients.
pub fn handle_payload(
    player: &Player,
    data: &[u8],
    server_version: Option<&str>,
    instance_id: &str,
) -> Outcome {
    let Some(hello) = protocol::parse_hello_c2s(data) else {
        return Outcome::NotHello {
            type_byte: data.first().copied(),
        };
    };

    let index = state::count_hello();
    let config = state::config_snapshot();
    let reply = build_reply(
        &config,
        server_version,
        instance_id,
        env!("CARGO_PKG_VERSION"),
        &hello,
    );
    let policy = reply.frames.last().cloned().unwrap_or_default();
    state::remember_policy(&policy);

    if !config.grants_seed() {
        warn!(
            "[confluxmap] no seed to grant ({}); replying with seedGranted=0 so the client is \
             not left waiting. Fill in `seed` in {} and run `/cfm reload`.",
            if config.seed.is_none() {
                "`seed` is unset or unparseable"
            } else {
                "share_seed = false"
            },
            crate::config::operator_path()
        );
    }

    let bytes: usize = reply.frames.iter().map(Vec::len).sum();
    let sent = reply.frames.iter().all(|frame| send(player, frame));

    let summary = summarise(player, &hello, &reply, &config, server_version, index, sent);

    if sent {
        state::count_policy_sent();
        info!("[confluxmap] {summary}");
        state::remember_handshake(summary.clone());
        Outcome::Replied { summary, bytes }
    } else {
        state::count_send_failure();
        warn!("[confluxmap] {summary}");
        state::remember_handshake(summary.clone());
        Outcome::SendFailed { summary }
    }
}

/// Sends a plugin message. Plugin messaging only exists on the Java side, so
/// this returns false for a Bedrock client.
pub fn send(player: &Player, data: &[u8]) -> bool {
    match player.as_java() {
        Some(java) => {
            java.send_custom_payload(protocol::CHANNEL_ID, data);
            true
        }
        None => false,
    }
}

fn summarise(
    player: &Player,
    hello: &HelloC2S,
    reply: &Reply,
    config: &Config,
    server_version: Option<&str>,
    index: u64,
    sent: bool,
) -> String {
    let client = match player.as_java() {
        Some(java) => format!(
            "client={:?} brand={:?}",
            java.get_version(),
            java.get_brand()
        ),
        None => "client=bedrock".to_string(),
    };
    let policy = reply.frames.last().map_or(0, Vec::len);
    format!(
        "HELLO #{index} {} modVersion={:?} predictor={:?} {client} -> {} frame(s) {}B \
         (caps2={} instance={}) HELLO_POLICY {}B seedGranted={} seed={} worldgen={:?} worldId={} \
         dims={}",
        player.get_name(),
        hello.mod_version,
        reply.offer.predictor,
        if sent { "sent" } else { "NOT sent" },
        reply.frames.iter().map(Vec::len).sum::<usize>(),
        reply.offer.caps2,
        reply.offer.server_instance,
        policy,
        config.grants_seed(),
        if config.grants_seed() {
            config.advertised_seed().to_string()
        } else {
            "-".to_string()
        },
        config.resolve_worldgen(server_version),
        config.world_id(),
        config.dims.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic seed, so no real world's seed ships in the repository.
    const SYNTHETIC_SEED: i64 = 0x0123_4567_89AB_CDEF;
    /// A synthetic instance id, for the same reason.
    const INSTANCE: &str = "00000000-0000-0000-0000-0000000000aa";

    fn seeded_config() -> Config {
        Config {
            seed: Some(SYNTHETIC_SEED),
            ..Config::default()
        }
    }

    fn hello_with(predictor: &str) -> HelloC2S {
        HelloC2S {
            mod_version: "0.3.0".to_string(),
            predictor_version: predictor.to_string(),
        }
    }

    /// The full pipeline, against the vector the reference encoder produced.
    #[test]
    fn build_policy_matches_the_reference_vector() {
        let policy = build_policy(&seeded_config(), Some("0.1.0-dev+26.2-26.45"));
        let golden = "0201002430303030303030302d303030302d303030302d303030302d343536373839616263646566000432362e320004000000080064040100136d696e6563726166743a6f766572776f726c6400096f766572776f726c64030123456789abcdef";
        assert_eq!(state::hex(&policy), golden);
    }

    /// A server whose version string cannot be parsed still resolves the
    /// worldgen version rather than emitting an empty one.
    #[test]
    fn unparseable_server_version_falls_back_cleanly() {
        let policy = build_policy(&seeded_config(), Some("garbage"));
        // worldgen sits after the worldId; check the fallback appears on the wire.
        let hex = state::hex(&policy);
        let expected_worldgen = state::hex(crate::config::FALLBACK_MC_VERSION.as_bytes());
        assert!(
            hex.contains(&expected_worldgen),
            "fallback worldgen {expected_worldgen} missing from {hex}"
        );
    }

    #[test]
    fn disabled_sharing_clears_the_seed_flag_and_zeroes_the_seed_on_the_wire() {
        let config = Config {
            seed: Some(7),
            share_seed: false,
            ..Config::default()
        };
        let withheld = build_policy(&config, None);
        assert_eq!(withheld[0], protocol::MSG_HELLO_POLICY_S2C);
        assert_eq!(
            withheld[1], 0,
            "no flags may be set when the seed is withheld"
        );
        // Same shape as a granting policy, so the client's parser sees the same
        // field layout; only the flag and the seed differ.
        assert_eq!(withheld.len(), build_policy(&seeded_config(), None).len());
        assert_eq!(
            &withheld[withheld.len() - 8..],
            &[0u8; 8],
            "the per-dim seed must be zero when the seed is withheld"
        );
    }

    /// A legacy client gets exactly the frame the previous release sent: adding
    /// negotiation must not put a new message id in front of a codec that
    /// rejects it.
    #[test]
    fn a_legacy_client_gets_only_the_policy() {
        let reply = build_reply(
            &seeded_config(),
            Some("0.1.0-dev+26.2-26.45"),
            INSTANCE,
            "0.1.1",
            &hello_with("probe-predictor-v0"),
        );
        assert!(!reply.offer.caps2);
        assert_eq!(reply.frames.len(), 1, "no selection, no instance frame");
        assert_eq!(reply.frames[0][0], protocol::MSG_HELLO_POLICY_S2C);
    }

    /// The whole point of the feature: a current client is told which server
    /// instance it is looking at, before the policy opens its session.
    #[test]
    fn a_capability_client_gets_selection_then_instance_then_policy() {
        let offer = "AgMDAgEIAQECAQMBBAEFAQYBBwEIAQ";
        let hello = hello_with(&format!("cubiomes-9f2c|sync:1|wire:4.0|caps2:{offer}"));
        let reply = build_reply(
            &seeded_config(),
            Some("0.1.0-dev+26.2-26.45"),
            INSTANCE,
            "0.1.1",
            &hello,
        );
        assert!(reply.offer.server_instance);
        let ids: Vec<u8> = reply.frames.iter().map(|frame| frame[0]).collect();
        assert_eq!(
            ids,
            vec![
                protocol::MSG_MAP_CAPABILITIES_S2C,
                protocol::MSG_SERVER_INSTANCE_S2C,
                protocol::MSG_HELLO_POLICY_S2C,
            ],
            "the client resolves its capabilities from the first frame"
        );

        // The selection must grant exactly the capability the instance frame
        // needs, and the instance frame must carry the configured id.
        let selection = &reply.frames[0];
        let tail = &selection[selection.len() - 3..];
        assert_eq!(state::hex(tail), "010701", "count, id, version");
        let instance = &reply.frames[1];
        assert!(
            state::hex(instance).contains(&state::hex(INSTANCE.as_bytes())),
            "the instance id must appear verbatim"
        );
    }

    /// An offer without capability 7 must not be answered with an instance
    /// frame: the client would reject a message it never selected.
    #[test]
    fn an_offer_without_the_capability_gets_a_selection_only() {
        let hello = hello_with("pred|caps2:AgEBAQEB");
        let reply = build_reply(&seeded_config(), None, INSTANCE, "0.1.1", &hello);
        assert!(reply.offer.caps2);
        assert!(!reply.offer.server_instance);
        let ids: Vec<u8> = reply.frames.iter().map(|frame| frame[0]).collect();
        assert_eq!(
            ids,
            vec![
                protocol::MSG_MAP_CAPABILITIES_S2C,
                protocol::MSG_HELLO_POLICY_S2C,
            ]
        );
        assert_eq!(
            *reply.frames[0].last().expect("non-empty"),
            0,
            "no capabilities"
        );
    }
}
