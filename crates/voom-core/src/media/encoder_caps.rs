//! Protocol-neutral encoder capability descriptors shared by policy validation,
//! planning, store-backed profile rows, and worker request validation.

/// How an encoder spells its speed knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetDomain {
    /// A fixed named set, e.g. x265 `ultrafast..placebo`.
    Named(&'static [&'static str]),
    /// An inclusive numeric range, e.g. SVT-AV1 `-preset 0..=13`.
    NumericRange { min: u8, max: u8 },
}

/// The encoder-specific constant-quality control accepted by a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityDomain {
    /// `FFmpeg`'s codec-independent constant rate factor.
    Crf { min: u8, max: u8 },
    /// NVIDIA's constant-quality target in VBR rate-control mode.
    Cq { min: u8, max: u8 },
    /// `VideoToolbox`'s target bitrate in kilobits per second.
    BitrateKbps { min: u32, max: u32 },
}

/// Whether an encoder executes in software or on a named accelerator backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoEncoderBackend {
    Software,
    Nvidia,
    VideoToolbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderDescriptor {
    pub encoder: &'static str,
    pub target_codec: &'static str,
    pub quality_domain: QualityDomain,
    pub backend: VideoEncoderBackend,
    pub preset_domain: PresetDomain,
    pub tunes: &'static [&'static str],
    pub codec_profiles: &'static [&'static str],
    pub codec_levels: &'static [&'static str],
    pub pixel_formats: &'static [&'static str],
    /// 10-bit pixel formats for this encoder (subset of `pixel_formats`).
    pub ten_bit_pixel_formats: &'static [&'static str],
    /// Codec profiles that only allow 8-bit pixel formats.
    pub eight_bit_only_profiles: &'static [&'static str],
    /// `libaom-av1` constant-quality mode requires `-b:v 0`.
    pub requires_bitrate_zero: bool,
}

const X265_PRESETS: &[&str] = &[
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
    "placebo",
];

const X265: EncoderDescriptor = EncoderDescriptor {
    encoder: "libx265",
    target_codec: "hevc",
    quality_domain: QualityDomain::Crf { min: 0, max: 51 },
    backend: VideoEncoderBackend::Software,
    preset_domain: PresetDomain::Named(X265_PRESETS),
    tunes: &["psnr", "ssim", "grain", "fastdecode", "zerolatency"],
    // V1: profiles requiring wider chroma/bit-depth deferred until their pixel formats are added.
    codec_profiles: &["main", "main10"],
    codec_levels: &[
        "3.0", "3.1", "4.0", "4.1", "5.0", "5.1", "5.2", "6.0", "6.1", "6.2",
    ],
    pixel_formats: &[
        "yuv420p",
        "yuv420p10le",
        "yuv422p",
        "yuv422p10le",
        "yuv444p",
        "yuv444p10le",
    ],
    ten_bit_pixel_formats: &["yuv420p10le", "yuv422p10le", "yuv444p10le"],
    eight_bit_only_profiles: &["main"],
    requires_bitrate_zero: false,
};

const SVTAV1: EncoderDescriptor = EncoderDescriptor {
    encoder: "libsvtav1",
    target_codec: "av1",
    quality_domain: QualityDomain::Crf { min: 0, max: 63 },
    backend: VideoEncoderBackend::Software,
    preset_domain: PresetDomain::NumericRange { min: 0, max: 13 },
    tunes: &["vq", "psnr"],
    codec_profiles: &["main"],
    codec_levels: &["4.0", "4.1", "5.0", "5.1", "6.0", "6.1"],
    pixel_formats: &["yuv420p", "yuv420p10le"],
    ten_bit_pixel_formats: &["yuv420p10le"],
    eight_bit_only_profiles: &[],
    requires_bitrate_zero: false,
};

const LIBAOM: EncoderDescriptor = EncoderDescriptor {
    encoder: "libaom-av1",
    target_codec: "av1",
    quality_domain: QualityDomain::Crf { min: 0, max: 63 },
    backend: VideoEncoderBackend::Software,
    preset_domain: PresetDomain::NumericRange { min: 0, max: 8 },
    tunes: &["psnr", "ssim"],
    // V1: profiles requiring wider chroma/bit-depth deferred until their pixel formats are added.
    codec_profiles: &["main"],
    codec_levels: &["4.0", "4.1", "5.0", "5.1", "6.0", "6.1"],
    pixel_formats: &["yuv420p", "yuv420p10le"],
    ten_bit_pixel_formats: &["yuv420p10le"],
    eight_bit_only_profiles: &[],
    requires_bitrate_zero: true,
};

const NVIDIA_PRESETS: &[&str] = &["p1", "p2", "p3", "p4", "p5", "p6", "p7"];

const HEVC_NVENC: EncoderDescriptor = EncoderDescriptor {
    encoder: "hevc_nvenc",
    target_codec: "hevc",
    quality_domain: QualityDomain::Cq { min: 1, max: 51 },
    backend: VideoEncoderBackend::Nvidia,
    preset_domain: PresetDomain::Named(NVIDIA_PRESETS),
    tunes: &["hq", "uhq", "ll", "ull", "lossless"],
    codec_profiles: &["main", "main10"],
    codec_levels: &[
        "3.0", "3.1", "4.0", "4.1", "5.0", "5.1", "5.2", "6.0", "6.1", "6.2",
    ],
    pixel_formats: &["yuv420p", "yuv420p10le"],
    ten_bit_pixel_formats: &["yuv420p10le"],
    eight_bit_only_profiles: &["main"],
    requires_bitrate_zero: true,
};

const H264_VIDEOTOOLBOX: EncoderDescriptor = EncoderDescriptor {
    encoder: "h264_videotoolbox",
    target_codec: "h264",
    quality_domain: QualityDomain::BitrateKbps {
        min: 1,
        max: u32::MAX,
    },
    backend: VideoEncoderBackend::VideoToolbox,
    preset_domain: PresetDomain::Named(&["default"]),
    tunes: &[],
    codec_profiles: &["high"],
    codec_levels: &["4.1"],
    pixel_formats: &["yuv420p"],
    ten_bit_pixel_formats: &[],
    eight_bit_only_profiles: &["high"],
    requires_bitrate_zero: false,
};

const HEVC_VIDEOTOOLBOX: EncoderDescriptor = EncoderDescriptor {
    encoder: "hevc_videotoolbox",
    target_codec: "hevc",
    quality_domain: QualityDomain::BitrateKbps {
        min: 1,
        max: u32::MAX,
    },
    backend: VideoEncoderBackend::VideoToolbox,
    preset_domain: PresetDomain::Named(&["default"]),
    tunes: &[],
    codec_profiles: &["main", "main10"],
    codec_levels: &[],
    pixel_formats: &["yuv420p", "yuv420p10le"],
    ten_bit_pixel_formats: &["yuv420p10le"],
    eight_bit_only_profiles: &["main"],
    requires_bitrate_zero: false,
};

const DESCRIPTORS: &[EncoderDescriptor] = &[
    X265,
    SVTAV1,
    LIBAOM,
    HEVC_NVENC,
    H264_VIDEOTOOLBOX,
    HEVC_VIDEOTOOLBOX,
];

pub const NVIDIA_VIDEO_DECODERS: &[(&str, &str)] = &[
    ("h264", "h264_cuvid"),
    ("hevc", "hevc_cuvid"),
    ("av1", "av1_cuvid"),
];

#[must_use]
pub fn encoder_descriptor(encoder: &str) -> Option<&'static EncoderDescriptor> {
    DESCRIPTORS.iter().find(|d| d.encoder == encoder)
}

#[must_use]
pub fn nvidia_decoder_for_video_codec(codec: &str) -> Option<&'static str> {
    let codec = if codec.eq_ignore_ascii_case("h265") {
        "hevc"
    } else {
        codec
    };
    NVIDIA_VIDEO_DECODERS
        .iter()
        .find(|(candidate, _)| codec.eq_ignore_ascii_case(candidate))
        .map(|(_, decoder)| *decoder)
}

impl EncoderDescriptor {
    #[must_use]
    pub const fn accepts_crf(&self, crf: u8) -> bool {
        let QualityDomain::Crf { min, max } = self.quality_domain else {
            return false;
        };
        crf >= min && crf <= max
    }

    #[must_use]
    pub const fn accepts_cq(&self, cq: u8) -> bool {
        let QualityDomain::Cq { min, max } = self.quality_domain else {
            return false;
        };
        cq >= min && cq <= max
    }

    #[must_use]
    pub const fn accepts_bitrate_kbps(&self, bitrate_kbps: u32) -> bool {
        let QualityDomain::BitrateKbps { min, max } = self.quality_domain else {
            return false;
        };
        bitrate_kbps >= min && bitrate_kbps <= max
    }

    #[must_use]
    pub fn accepts_preset(&self, preset: &str) -> bool {
        match self.preset_domain {
            PresetDomain::Named(set) => set.contains(&preset),
            PresetDomain::NumericRange { min, max } => preset
                .parse::<u8>()
                .is_ok_and(|value| value >= min && value <= max),
        }
    }

    #[must_use]
    pub fn accepts_tune(&self, tune: &str) -> bool {
        self.tunes.contains(&tune)
    }

    #[must_use]
    pub fn accepts_codec_profile(&self, profile: &str) -> bool {
        self.codec_profiles.contains(&profile)
    }

    #[must_use]
    pub fn accepts_codec_level(&self, level: &str) -> bool {
        self.codec_levels.contains(&level)
    }

    #[must_use]
    pub fn accepts_pixel_format(&self, pixel_format: &str) -> bool {
        self.pixel_formats.contains(&pixel_format)
    }

    /// A 10-bit pixel format is incompatible with an 8-bit-only codec profile.
    #[must_use]
    pub fn pixel_format_compatible_with_profile(
        &self,
        pixel_format: &str,
        codec_profile: Option<&str>,
    ) -> bool {
        let Some(profile) = codec_profile else {
            return true;
        };
        if !self.eight_bit_only_profiles.contains(&profile) {
            return true;
        }
        !self.ten_bit_pixel_formats.contains(&pixel_format)
    }
}

#[cfg(test)]
#[path = "encoder_caps_test.rs"]
mod tests;
