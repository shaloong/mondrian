//! Shared, test-only observations for finite external-vendor controls.
//! Both consumers use the public Audio API and retain the original file/owner rules.

use super::audio::{
    self, AudioDecodedSource, AudioMediaResolver, AudioProgramRuntime, ClapParameterDescriptor,
    ResolvedAudioSource, Vst3ParameterDescriptor,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioSourceComponentId, ExecutionCancellationToken, ParameterId,
};
use mondrian_timeline::audio::AudioProcessorInstance;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub(super) const FRAMES: usize = 4096;
pub(super) const MAX_JSON: u64 = 512 * 1024;

pub(super) fn write_json(directory: &Path, name: &str, value: Value) {
    let bytes = serde_json::to_vec_pretty(&value).expect("complete bounded PCM observation");
    assert!(bytes.len() as u64 <= MAX_JSON, "bounded JSON evidence");
    write_new(directory, name, &bytes);
}

// A bound source in the normal media resolver. It has no plugin/output access.
// Repeat the independently pinned 4096-frame input at exact sample positions.
pub(super) struct PcmMedia {
    pub(super) period: Arc<[f32]>,
    pub(super) reads: Arc<AtomicUsize>,
}
impl AudioMediaResolver for PcmMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        if rate != 48_000 {
            return Err("bound PCM source rate".to_owned());
        }
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Stereo,
            Arc::new(PcmMedia {
                period: self.period.clone(),
                reads: self.reads.clone(),
            }),
        ))
    }
}
impl AudioDecodedSource for PcmMedia {
    fn read_interleaved(
        &self,
        start: i64,
        frames: usize,
        out: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        if cancellation.is_canceled() || out.len() != frames * 2 {
            return Err("bound PCM source cancellation/extent".to_owned());
        }
        self.reads.fetch_add(1, Ordering::Relaxed);
        for (offset, pair) in out.chunks_exact_mut(2).enumerate() {
            let sample = start.checked_add(offset as i64).ok_or("source time overflow")?;
            if sample < 0 {
                pair.fill(0.0);
            } else {
                let i = (sample as usize % FRAMES) * 2;
                pair.copy_from_slice(&self.period[i..i + 2]);
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Cursors {
    pub(super) sessions: Vec<audio::AudioSessionCursorEvidence>,
    pub(super) processors: Vec<audio::AudioProcessorCursorEvidence>,
}
impl audio::AudioContinuityEvidenceVisitor for Cursors {
    fn session(
        &mut self,
        evidence: audio::AudioSessionCursorEvidence,
    ) -> Result<(), audio::AudioExecutionError> {
        self.sessions.push(evidence);
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: audio::AudioProcessorCursorEvidence,
    ) -> Result<(), audio::AudioExecutionError> {
        self.processors.push(evidence);
        Ok(())
    }
}
pub(super) fn owner(runtime: &AudioProgramRuntime) -> audio::AudioNativeProcessorOwnerLease {
    let mut cursors = Cursors::default();
    runtime.visit_continuity_evidence(&mut cursors).expect("actual Runtime cursors");
    assert_eq!((cursors.sessions.len(), cursors.processors.len()), (1, 1));
    let processor = cursors.processors[0];
    let epoch = match processor.state {
        audio::AudioProcessorCursorState::Unentered => None,
        audio::AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
        _ => panic!("actual native occurrence has no usable cold/active owner"),
    };
    runtime
        .native_owner_lease(audio::AudioNativeProcessorAddress {
            root: cursors.sessions[0].identity,
            local: processor.identity,
            occurrence: processor.occurrence,
            epoch,
        })
        .expect("actual installed Runtime-issued native lease")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub(super) path: PathBuf,
    pub(super) bytes: u64,
    pub(super) sha256: String,
}

// serde_json::Value normally accepts duplicate keys. Reject them recursively
// before typed deserialization, with serde_json's default depth/number limits.
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Strict;
        impl<'de> Visitor<'de> for Strict {
            type Value = StrictValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("strict JSON")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| StrictValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictValue(v)) = a.next_element()? {
                    values.push(v);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((k, StrictValue(v))) = a.next_entry::<String, StrictValue>()? {
                    if values.insert(k.clone(), v).is_some() {
                        return Err(de::Error::custom(format!("duplicate key {k}")));
                    }
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        d.deserialize_any(Strict)
    }
}

pub(super) fn strict_json(bytes: &[u8]) -> Value {
    assert!(bytes.len() as u64 <= MAX_JSON, "JSON capacity");
    assert!(std::str::from_utf8(bytes).is_ok(), "UTF-8 JSON");
    serde_json::from_slice::<StrictValue>(bytes).expect("strict complete JSON").0
}

pub(super) fn exact_keys(value: &Value, fields: &[&str]) {
    let actual: BTreeSet<_> =
        value.as_object().expect("JSON object").keys().map(String::as_str).collect();
    assert_eq!(
        actual,
        fields.iter().copied().collect(),
        "unknown/missing captured metadata field"
    );
}

pub(super) fn no_reparse(path: &Path) {
    assert!(path.is_absolute(), "external path must be absolute");
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let m = fs::symlink_metadata(ancestor).expect("existing external ancestor");
        assert_eq!(m.file_attributes() & 0x400, 0, "reparse external ancestor");
    }
}

pub(super) struct Lease {
    file: File,
    pub(super) path: PathBuf,
    pub(super) sha256: String,
    pub(super) bytes: u64,
}
impl Lease {
    pub(super) fn open(binding: &Binding, maximum: u64) -> Self {
        no_reparse(&binding.path);
        assert!(
            binding.bytes > 0 && binding.bytes <= maximum,
            "bound file capacity"
        );
        assert!(
            binding.sha256.len() == 64
                && binding.sha256.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "lowercase SHA256"
        );
        let path = fs::canonicalize(&binding.path).expect("canonical external path");
        // FILE_SHARE_READ denies concurrent writes/delete; root additionally
        // owns directory/source/artifact leases and the outer process Job.
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .expect("read-only external lease");
        let metadata = file.metadata().expect("leased file metadata");
        assert!(metadata.is_file());
        assert_eq!(metadata.len(), binding.bytes, "exact external byte count");
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut size = 0_u64;
        loop {
            let n = file.read(&mut buffer).expect("hash leased bytes");
            if n == 0 {
                break;
            }
            size += n as u64;
            assert!(size <= binding.bytes);
            hash.update(&buffer[..n]);
        }
        assert_eq!(size, binding.bytes);
        let sha256 = format!("{:x}", hash.finalize());
        assert_eq!(sha256, binding.sha256, "exact external SHA256");
        assert_eq!(
            fs::canonicalize(&binding.path).expect("still canonical"),
            path
        );
        Self { file, path, sha256, bytes: size }
    }
    pub(super) fn read(&mut self) -> Vec<u8> {
        self.file.seek(SeekFrom::Start(0)).expect("rewind retained lease");
        let mut bytes = Vec::with_capacity(self.bytes as usize);
        Read::by_ref(&mut self.file)
            .take(self.bytes + 1)
            .read_to_end(&mut bytes)
            .expect("read bounded retained bytes");
        assert_eq!(bytes.len() as u64, self.bytes);
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), self.sha256);
        bytes
    }
}

pub(super) enum Facts {
    Clap(Vec<ClapParameterDescriptor>),
    #[allow(
        dead_code,
        reason = "The unit-test consumer also validates VST3; the integration consumer is CLAP-only."
    )]
    Vst3(Vec<Vst3ParameterDescriptor>, Vec<(u32, f64)>),
}
impl Facts {
    pub(super) fn check_author(&self, instance: &AudioProcessorInstance) {
        self.check_author_profile(instance, false);
    }

    pub(super) fn check_native_owned_author(&self, instance: &AudioProcessorInstance) {
        assert!(
            instance.opaque_state.is_some(),
            "known stateful factory capture"
        );
        self.check_author_profile(instance, true);
    }

    // The caller chooses a known construction contract; opaque bytes never
    // infer the ownership of an arbitrary existing or explicitly edited author.
    fn check_author_profile(&self, instance: &AudioProcessorInstance, native_owned: bool) {
        let count = match self {
            Self::Clap(p) => p.len(),
            Self::Vst3(p, _) => p.len(),
        };
        assert_eq!(
            instance.parameters.len(),
            count,
            "first slice admits only editable captured parameters"
        );
        let mut ids = BTreeSet::new();
        let mut check = |id: ParameterId, schema, value: f64, name: &str, hidden: bool| {
            assert!(
                ids.insert(id.clone()),
                "unique unsigned native parameter IDs"
            );
            let actual = instance.parameters.get(&id).expect("captured canonical parameter");
            assert_eq!(actual.schema, schema, "native schema after restore");
            assert_eq!(actual.ui.display_name.as_deref(), Some(name));
            assert_eq!(actual.ui.hidden, hidden);
            assert!(value.is_finite(), "actual current scalar is finite");
            if native_owned {
                assert!(
                    actual.automation.is_none(),
                    "native state owns each initial value"
                );
            } else {
                let curve = actual.automation.as_ref().expect("explicit constant author value");
                assert_eq!(curve.parameter_id, id);
                assert!(curve.keyframes.is_empty());
                assert_eq!(
                    curve.default_value.to_bits(),
                    value.to_bits(),
                    "explicit current value"
                );
            }
        };
        match self {
            Self::Clap(p) => {
                for d in p {
                    assert!(!d.read_only, "readonly cases need a later explicit slice");
                    assert!(
                        d.current_value.is_finite()
                            && (d.min_value..=d.max_value).contains(&d.current_value)
                            && (!d.stepped || d.current_value.fract() == 0.0)
                    );
                    check(
                        d.parameter_id().expect("CLAP unsigned ID"),
                        d.authoring_schema().expect("CLAP schema"),
                        d.current_value,
                        &d.name,
                        d.hidden,
                    );
                }
            }
            Self::Vst3(p, current) => {
                assert_eq!(p.len(), current.len());
                for (d, (id, v)) in p.iter().zip(current) {
                    assert_eq!(d.id, *id);
                    assert!(!d.read_only, "readonly cases need a later explicit slice");
                    check(
                        d.parameter_id().expect("VST3 unsigned ID"),
                        d.authoring_schema().expect("VST3 schema"),
                        d.author_scalar_from_normalized(*v).expect("normalized current"),
                        &d.name,
                        false,
                    );
                }
            }
        }
    }
    /// Compare independent actual restored queries, never an author curve.
    pub(super) fn check_native_current(&self, actual: &Self) {
        match (self, actual) {
            (Self::Clap(expected), Self::Clap(actual)) => {
                assert_eq!(actual, expected, "full native restored metadata");
                for (actual, expected) in actual.iter().zip(expected) {
                    assert_eq!(
                        actual.current_value.to_bits(),
                        expected.current_value.to_bits()
                    );
                }
            }
            (Self::Vst3(expected, values), Self::Vst3(actual, current)) => {
                assert_eq!(actual, expected, "full native restored metadata");
                assert_eq!(current.len(), values.len());
                for ((id, value), (expected_id, expected_value)) in current.iter().zip(values) {
                    assert_eq!(id, expected_id);
                    assert_eq!(
                        value.to_bits(),
                        expected_value.to_bits(),
                        "actual restored current"
                    );
                }
            }
            _ => panic!("native current query format changed"),
        }
    }
}

pub(super) fn write_new(dir: &Path, name: &str, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(name))
        .expect("create-only evidence");
    file.write_all(bytes).expect("write evidence bytes");
    file.sync_all().expect("flush evidence");
}

pub(super) fn deterministic_pcm() -> Vec<f32> {
    (0..FRAMES)
        .flat_map(|i| {
            let left = if i == 0 {
                0.25
            } else {
                ((i * 17 % 255) as i32 - 127) as f32 / 512.0
            };
            let right = ((i * 29 % 253) as i32 - 126) as f32 / 512.0;
            [left, if i % 2 == 0 { right } else { -right }]
        })
        .collect()
}
pub(super) fn pcm_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|v| v.to_le_bytes()).collect()
}

pub(super) fn decode_hash(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| {
            u8::from_str_radix(std::str::from_utf8(p).expect("hash ASCII"), 16).expect("hex hash")
        })
        .collect()
}
