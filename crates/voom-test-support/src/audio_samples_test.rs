#![expect(clippy::unwrap_used, reason = "generated media test assertions")]

use super::*;
use std::path::Path;
use tokio::process::Command;

fn integer_wave(path: &Path, bits: u16, changed: bool) -> Vec<i32> {
    let limit = 1_i32 << (bits - 1);
    let mut values: Vec<i32> = [-limit, limit - 1, -1, 1, 0, 123, -123, 3]
        .into_iter()
        .cycle()
        .take(256)
        .collect();
    if changed {
        values[4] ^= 1;
    }
    let data: Vec<u8> = values
        .iter()
        .flat_map(|value| value.to_le_bytes()[..usize::from(bits / 8)].to_vec())
        .collect();
    // Extensible WAV retains an explicit stereo mask even for 16-bit fixtures.
    let mut fmt = Vec::new();
    for bytes in [0xfffe_u16.to_le_bytes(), 2_u16.to_le_bytes()] {
        fmt.extend(bytes);
    }
    fmt.extend(48_000_u32.to_le_bytes());
    fmt.extend((48_000_u32 * 2 * u32::from(bits / 8)).to_le_bytes());
    for value in [2 * (bits / 8), bits, 22, bits] {
        fmt.extend(value.to_le_bytes());
    }
    fmt.extend(3_u32.to_le_bytes());
    fmt.extend([1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
    let mut body = b"WAVEfmt ".to_vec();
    body.extend(40_u32.to_le_bytes());
    body.extend(fmt);
    body.extend(b"data");
    body.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    body.extend(data);
    let mut wave = b"RIFF".to_vec();
    wave.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
    wave.extend(body);
    std::fs::write(path, wave).unwrap();
    values
}

#[tokio::test]
async fn canonical_audio_preserves_integer_lsb_and_rejects_one_bit_change() {
    let dir = tempfile::tempdir().unwrap();
    for bits in [16, 24] {
        let source = dir.path().join(format!("source-{bits}.wav"));
        let changed = dir.path().join(format!("changed-{bits}.wav"));
        let encoded = dir.path().join(format!("encoded-{bits}.flac"));
        let values = integer_wave(&source, bits, false);
        integer_wave(&changed, bits, true);
        encode_fixture(&source, &encoded, "flac").await;
        let original =
            canonical_decoded_audio(Path::new("ffmpeg"), Path::new("ffprobe"), &source, 0)
                .await
                .unwrap();
        let actual =
            canonical_decoded_audio(Path::new("ffmpeg"), Path::new("ffprobe"), &encoded, 0)
                .await
                .unwrap();
        let different =
            canonical_decoded_audio(Path::new("ffmpeg"), Path::new("ffprobe"), &changed, 0)
                .await
                .unwrap();
        let expected: Vec<u8> = values
            .iter()
            .flat_map(|v| (v << (32 - bits)).to_le_bytes())
            .collect();
        assert_eq!(
            original.samples, expected,
            "canonicalization retains low source bits"
        );
        assert_eq!(original.facts.bits_per_sample, u8::try_from(bits).unwrap());
        original.compare_samples(&actual).unwrap();

        assert!(
            original
                .compare_samples(&different)
                .unwrap_err()
                .to_string()
                .contains("samples differ")
        );
        for mutation in ["rate", "layout", "channels"] {
            let mut wrong = actual.clone();
            match mutation {
                "rate" => wrong.facts.sample_rate = 44_100,
                "layout" => wrong.facts.channel_layout = "mono".to_owned(),
                _ => wrong.facts.channels = 1,
            }
            assert!(original.compare_samples(&wrong).is_err(), "{mutation}");
        }
        assert!(
            canonical_decoded_audio(Path::new("ffmpeg"), Path::new("ffprobe"), &source, 9)
                .await
                .is_err()
        );
        assert!(
            canonical_decoded_audio(
                &dir.path().join("missing-tool"),
                Path::new("ffprobe"),
                &source,
                0
            )
            .await
            .is_err()
        );
    }
}

async fn encode_fixture(source: &Path, output: &Path, codec: &str) {
    command_output(
        Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-n", "-i"])
            .arg(source)
            .args(["-c:a", codec])
            .arg(output),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn canonical_audio_allows_padding_and_rejects_float_before_decoding() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.wav");
    integer_wave(&source, 16, false);
    let widened = dir.path().join("widened.wav");
    encode_fixture(&source, &widened, "pcm_s24le").await;
    let original = canonical_decoded_audio(Path::new("ffmpeg"), Path::new("ffprobe"), &source, 0)
        .await
        .unwrap();
    let padded = canonical_decoded_audio(Path::new("ffmpeg"), Path::new("ffprobe"), &widened, 0)
        .await
        .unwrap();
    assert_eq!(padded.facts.bits_per_sample, 24);
    original.compare_samples(&padded).unwrap();
    let floating = dir.path().join("floating.wav");
    encode_fixture(&source, &floating, "pcm_f32le").await;
    let error = canonical_decoded_audio(
        &dir.path().join("missing-decoder"),
        Path::new("ffprobe"),
        &floating,
        0,
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("16/24-bit integer precision"),
        "{error}"
    );
}
