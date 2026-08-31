//! History/export snapshot codec (§6.6, §10.1): the full project model as
//! zstd-compressed JSON, wrapped in a versioned envelope so restored
//! snapshots can be migrated like project files (§8.1).

use serde::{Deserialize, Serialize};

use crate::model::Project;
use crate::MODEL_VERSION;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("snapshot compression failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("snapshot is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("snapshot model version {0} is newer than this app understands ({MODEL_VERSION})")]
    TooNew(u32),
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    model: Project,
}

/// Serialize + compress a project snapshot.
pub fn encode(project: &Project) -> Result<Vec<u8>, SnapshotError> {
    let env = Envelope {
        version: MODEL_VERSION,
        model: project.clone(),
    };
    let json = serde_json::to_vec(&env)?;
    Ok(zstd::encode_all(json.as_slice(), 3)?)
}

/// Decompress + deserialize a snapshot, refusing newer-than-us versions
/// (§8.1: never write/interpret a file we don't fully understand).
pub fn decode(blob: &[u8]) -> Result<Project, SnapshotError> {
    let json = zstd::decode_all(blob)?;
    // Peek the version before deserializing the model, so a future-versioned
    // model shape fails with TooNew rather than a JSON error.
    #[derive(Deserialize)]
    struct VersionOnly {
        version: u32,
    }
    let v: VersionOnly = serde_json::from_slice(&json)?;
    if v.version > MODEL_VERSION {
        return Err(SnapshotError::TooNew(v.version));
    }
    // Forward migrations for old snapshot versions slot in here (§8.1).
    let env: Envelope = serde_json::from_slice(&json)?;
    Ok(env.model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Media, MediaId, MediaKind};

    #[test]
    fn roundtrip() {
        let mut p = Project::new("roundtrip", 123);
        let id = MediaId(p.alloc_id());
        p.media.push(Media {
            id,
            path: "/tmp/a.mp4".into(),
            hash: "abc".into(),
            kind: MediaKind::Video,
            duration_us: Some(1),
            video_codec: None,
            audio_codec: None,
            width: None,
            height: None,
            fps_num: None,
            fps_den: None,
            added_at: 5,
            offline: true, // runtime-only — must NOT survive the roundtrip
        });
        let blob = encode(&p).expect("encode");
        let back = decode(&blob).expect("decode");
        assert_eq!(back.meta, p.meta);
        assert_eq!(back.media[0].hash, "abc");
        assert!(!back.media[0].offline, "offline flag is runtime-only");
    }

    #[test]
    fn newer_version_refused() {
        let p = Project::new("v", 0);
        let json = serde_json::to_vec(&serde_json::json!({
            "version": MODEL_VERSION + 1,
            "model": serde_json::to_value(&p).expect("to_value"),
        }))
        .expect("json");
        let blob = zstd::encode_all(json.as_slice(), 3).expect("zstd");
        match decode(&blob) {
            Err(SnapshotError::TooNew(v)) => assert_eq!(v, MODEL_VERSION + 1),
            other => panic!("expected TooNew, got {other:?}"),
        }
    }
}
