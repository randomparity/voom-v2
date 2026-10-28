#![expect(clippy::unwrap_used, reason = "behavioral contract test assertions")]

use super::*;
use serde_json::json;

fn source(bits: u8) -> AudioSampleFacts {
    AudioSampleFacts::from_ffprobe(&json!({
        "sample_rate": "48000", "sample_fmt": if bits == 16 { "s16" } else { "s32p" },
        "bits_per_raw_sample": bits.to_string(), "bits_per_sample": bits,
        "channels": 2, "channel_layout": "stereo"
    }))
    .unwrap()
}

#[test]
fn archival_resolution_requires_integer_evidence_and_prevents_reduction() {
    let preserve = LosslessAudioSettings::default();
    for bits in [16, 24] {
        let source = source(bits);
        let resolved = preserve.resolve(&source).unwrap();
        assert_eq!(resolved.bits_per_sample, bits);
        assert_eq!(resolved.sample_rate, 48_000);
        assert_eq!(resolved.channel_layout, "stereo");
        resolved.verify(&source).unwrap();
        for field in ["rate", "format", "precision", "layout", "channels"] {
            let mut invalid = source.clone();
            match field {
                "rate" => invalid.sample_rate = None,
                "format" => invalid.sample_format = Some("fltp".to_owned()),
                "precision" => invalid.bits_per_sample = None,
                "layout" => invalid.channel_layout = None,
                _ => invalid.channels = None,
            }
            assert!(preserve.resolve(&invalid).is_err(), "required {field}");
            assert!(
                resolved.verify(&invalid).is_err(),
                "verify requires {field}"
            );
        }
    }
    assert!(
        LosslessAudioSettings {
            bits_per_sample: Some(16)
        }
        .resolve(&source(24))
        .is_err()
    );
    assert_eq!(
        LosslessAudioSettings {
            bits_per_sample: Some(24)
        }
        .resolve(&source(16))
        .unwrap()
        .bits_per_sample,
        24
    );
    for bits in [0, 8, 32] {
        let mut invalid = source(24);
        invalid.bits_per_sample = Some(bits);
        assert!(preserve.resolve(&invalid).is_err());
    }
    let mut invalid = source(24);
    invalid.sample_format = Some("s16".to_owned());
    assert!(preserve.resolve(&invalid).is_err());
    let mut invalid = source(16);
    invalid.sample_rate = Some(0);
    assert!(preserve.resolve(&invalid).is_err());
}

#[test]
fn archival_verification_preserves_exact_topology_and_precision() {
    for (channels, layout) in [
        (1, "mono"),
        (2, "stereo"),
        (6, "5.1"),
        (6, "5.1(side)"),
        (8, "7.1"),
    ] {
        let mut facts = source(24);
        facts.channels = Some(channels);
        facts.channel_layout = Some(layout.to_owned());
        let resolved = LosslessAudioSettings::default().resolve(&facts).unwrap();
        resolved.verify(&facts).unwrap();
        let mut wrong = facts.clone();
        wrong.channels = Some(channels + 1);
        assert!(resolved.verify(&wrong).is_err());
        let mut wrong = facts.clone();
        wrong.sample_rate = Some(44_100);
        assert!(resolved.verify(&wrong).is_err());
        let mut wrong = facts.clone();
        wrong.bits_per_sample = Some(16);
        assert!(resolved.verify(&wrong).is_err());
        let mut wrong = facts;
        wrong.channel_layout = Some("unknown".to_owned());
        assert!(resolved.verify(&wrong).is_err());
    }
    let mut back = source(24);
    back.channels = Some(6);
    back.channel_layout = Some("5.1".to_owned());
    let resolved = LosslessAudioSettings::default().resolve(&back).unwrap();
    back.channel_layout = Some("5.1(side)".to_owned());
    assert!(
        resolved.verify(&back).is_err(),
        "equal channel counts do not prove channel order"
    );
}

#[test]
fn probe_precision_uses_observed_evidence() {
    let facts = AudioSampleFacts::from_ffprobe(&json!({
        "sample_rate":"48000", "sample_fmt":"s32", "bits_per_raw_sample":"24", "bits_per_sample":0
    }))
    .unwrap();
    assert_eq!(facts.bits_per_sample, Some(24));
    let unknown =
        AudioSampleFacts::from_ffprobe(&json!({"sample_fmt":"s32", "bits_per_sample":0})).unwrap();
    assert_eq!(
        unknown.bits_per_sample, None,
        "storage width is not significant precision"
    );
    for invalid in [
        json!({"sample_rate": -1}),
        json!({"sample_rate":"4294967296"}),
        json!({"bits_per_raw_sample":"not-a-number"}),
        json!({"bits_per_raw_sample":24,"bits_per_sample":16,"sample_fmt":"s32"}),
        json!({"bits_per_raw_sample":24,"bits_per_sample":40,"sample_fmt":"s32"}),
        json!({"bits_per_raw_sample":24,"bits_per_sample":256,"sample_fmt":"s32"}),
        json!({"bits_per_sample":24,"sample_fmt":"s16"}),
    ] {
        assert!(
            AudioSampleFacts::from_ffprobe(&invalid).is_err(),
            "{invalid}"
        );
    }
}
