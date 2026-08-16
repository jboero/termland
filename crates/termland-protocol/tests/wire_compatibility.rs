//! Cross-version wire compatibility, against bytes real releases actually sent.
//!
//! The `old_peer_*` tests in `messages.rs` model an older peer by hand-writing
//! a struct without the newer fields. That proves how serde behaves, but the
//! struct is a *reconstruction* of what old code sent — if the reconstruction
//! is wrong, the test passes while real interop breaks, and nothing catches it.
//!
//! These fixtures cannot be wrong about it. Each file under `fixtures/<tag>/`
//! was produced by building that tag's own `termland-protocol` and asking it to
//! serialise the message. See `packaging/gen-protocol-fixtures.sh`.
//!
//! Only the *decode* direction is covered here, i.e. a current build reading
//! what an older peer sends. That is the case that matters in practice: servers
//! are upgraded before the clients connecting to them, and an Android client
//! may lag by months. The reverse (an old peer reading current bytes) cannot be
//! tested from inside this build — it needs the old crate — and is covered
//! structurally by `structs_are_encoded_as_name_keyed_maps`, which pins the
//! property that makes appending a field safe at all.

use std::path::{Path, PathBuf};

use termland_protocol::{AudioCodec, Message, MessageId, VideoCodec};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every `(tag, file, bytes)` fixture, sorted so failures name a stable case.
fn all_fixtures() -> Vec<(String, String, Vec<u8>)> {
    let mut out = Vec::new();
    let root = fixtures_root();
    let mut tags: Vec<_> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("no fixtures at {}: {e}", root.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    tags.sort();

    for tag_dir in tags {
        let tag = tag_dir.file_name().unwrap().to_string_lossy().to_string();
        let mut files: Vec<_> = std::fs::read_dir(&tag_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "cbor"))
            .collect();
        files.sort();
        for f in files {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            out.push((tag.clone(), name, std::fs::read(&f).unwrap()));
        }
    }
    out
}

/// The fixtures must actually be there. Without this, a mis-set path would make
/// every test below vacuously pass over an empty list.
#[test]
fn fixtures_exist_for_multiple_releases() {
    let fixtures = all_fixtures();
    assert!(!fixtures.is_empty(), "no fixtures found — did the generator run?");

    let tags: std::collections::BTreeSet<_> = fixtures.iter().map(|(t, _, _)| t).collect();
    assert!(
        tags.len() >= 5,
        "expected fixtures from several releases, got {tags:?}",
    );
}

/// Nothing any released version sent may fail to decode. A failure here means
/// a current build would drop the connection of a peer running that release.
#[test]
fn every_released_version_still_decodes() {
    for (tag, name, bytes) in all_fixtures() {
        match Message::decode(&bytes) {
            Ok(_) => {}
            Err(e) => panic!(
                "{tag}/{name}.cbor no longer decodes: {e}\n\
                 A peer running {tag} would be disconnected by this build.",
            ),
        }
    }
}

/// Decoding must also produce the *right message*, not merely succeed. A
/// message id that shifted would decode into the wrong variant rather than
/// erroring, which is far worse than a clean failure.
#[test]
fn fixtures_decode_to_the_expected_message_type() {
    for (tag, name, bytes) in all_fixtures() {
        let msg = Message::decode(&bytes).unwrap_or_else(|e| panic!("{tag}/{name}: {e}"));
        let expected = match name.as_str() {
            "hello" => MessageId::Hello,
            "session_create" => MessageId::SessionCreate,
            "session_list" => MessageId::SessionList,
            other => panic!("unknown fixture {other} — add it to this match"),
        };
        assert_eq!(
            msg.message_id(),
            expected,
            "{tag}/{name} decoded as the wrong message type",
        );
    }
}

/// The compatibility rule that is easiest to get wrong, checked against real
/// bytes rather than a hand-written stand-in.
///
/// No release before v0.7.0 sent `supported_audio_codecs`. Every one of them
/// must be read as Opus-only. If this ever comes back as the full preferred
/// set, the default has been changed to `all_preferred()` and the next audio
/// codec added will be offered to clients that cannot decode it — audio that
/// connects and plays silence.
#[test]
fn pre_v0_7_clients_are_read_as_opus_only() {
    let mut checked = 0;
    for (tag, name, bytes) in all_fixtures() {
        if name != "session_create" || tag.as_str() >= "v0.7.0" {
            continue;
        }
        let Ok(Message::SessionCreate(sc)) = Message::decode(&bytes) else {
            panic!("{tag}/{name} did not decode as SessionCreate");
        };
        assert_eq!(
            sc.supported_audio_codecs,
            vec![AudioCodec::Opus],
            "{tag} predates audio negotiation and must be treated as Opus-only",
        );
        checked += 1;
    }
    assert!(checked >= 4, "expected several pre-v0.7.0 fixtures, saw {checked}");
}

/// v0.3.x predates video codec negotiation entirely. Those clients must be
/// read as supporting everything — the opposite default to audio, because at
/// the time every codec *was* supported. Getting this backwards would refuse
/// to serve a v0.3 client at all.
#[test]
fn pre_negotiation_clients_are_read_as_supporting_all_video_codecs() {
    let mut checked = 0;
    for (tag, name, bytes) in all_fixtures() {
        if name != "session_create" || !tag.starts_with("v0.3") {
            continue;
        }
        let Ok(Message::SessionCreate(sc)) = Message::decode(&bytes) else {
            panic!("{tag}/{name} did not decode as SessionCreate");
        };
        assert_eq!(
            sc.supported_codecs,
            VideoCodec::all_preferred(),
            "{tag} predates codec negotiation and must be treated as supporting all",
        );
        checked += 1;
    }
    assert!(checked >= 1, "expected v0.3.x fixtures, saw {checked}");
}

/// Fields that existed all along must survive unchanged. This catches a field
/// being renamed or retyped, which serde would otherwise paper over by
/// substituting the default and losing the client's actual request.
#[test]
fn long_standing_fields_are_preserved_across_versions() {
    for (tag, name, bytes) in all_fixtures() {
        if name != "session_create" {
            continue;
        }
        let Ok(Message::SessionCreate(sc)) = Message::decode(&bytes) else {
            panic!("{tag}/{name} did not decode as SessionCreate");
        };
        assert_eq!(sc.width, 1920, "{tag}: width was lost");
        assert_eq!(sc.height, 1080, "{tag}: height was lost");
        assert!(sc.audio, "{tag}: audio flag was lost");
        assert_eq!(sc.quality, 75, "{tag}: quality was lost");
    }
}

/// Every release has spoken protocol version 1. If a fixture ever disagrees,
/// either the constant changed without a wire break being intended, or one
/// genuinely happened and this suite needs a policy rather than an assert.
#[test]
fn protocol_version_has_never_changed() {
    for (tag, name, bytes) in all_fixtures() {
        if name != "hello" {
            continue;
        }
        let Ok(Message::Hello(h)) = Message::decode(&bytes) else {
            panic!("{tag}/{name} did not decode as Hello");
        };
        assert_eq!(
            h.protocol_version, 1,
            "{tag} announced protocol version {}, not 1",
            h.protocol_version,
        );
    }
}
