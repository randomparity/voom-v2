//! Typed video-accelerator capability, requirement, and assignment vocabulary.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NvidiaVideoAcceleratorDescriptor {
    pub hardware_token: String,
    pub device_uuid: String,
    pub device_name: String,
    pub driver_version: String,
    pub encoders: Vec<String>,
    pub decoders: Vec<String>,
    pub max_sessions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoToolboxDecodeCapability {
    pub codec: String,
    pub pixel_formats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoToolboxVideoAcceleratorDescriptor {
    pub hardware_token: String,
    pub resource_id: String,
    pub model_identifier: String,
    pub chip_name: String,
    pub macos_version: String,
    pub macos_build: String,
    pub encoders: Vec<String>,
    pub decoders: Vec<VideoToolboxDecodeCapability>,
    pub max_sessions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoAcceleratorDescriptor {
    Nvidia(NvidiaVideoAcceleratorDescriptor),
    VideoToolbox(VideoToolboxVideoAcceleratorDescriptor),
}

impl VideoAcceleratorDescriptor {
    #[must_use]
    pub fn hardware_token(&self) -> &str {
        match self {
            Self::Nvidia(value) => &value.hardware_token,
            Self::VideoToolbox(value) => &value.hardware_token,
        }
    }

    #[must_use]
    pub const fn max_sessions(&self) -> u32 {
        match self {
            Self::Nvidia(value) => value.max_sessions,
            Self::VideoToolbox(value) => value.max_sessions,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalWorkerBound {
    pub addr: SocketAddr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<VideoAcceleratorDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoftwareVideoHardwareRequirement {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NvidiaVideoHardwareRequirement {
    pub encoder: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoToolboxDecodeRequirement {
    pub codec: String,
    pub pixel_format: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoToolboxVideoHardwareRequirement {
    pub encoder: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoder: Option<VideoToolboxDecodeRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoHardwareRequirement {
    Software(SoftwareVideoHardwareRequirement),
    Nvidia(NvidiaVideoHardwareRequirement),
    VideoToolbox(VideoToolboxVideoHardwareRequirement),
}

impl VideoHardwareRequirement {
    #[must_use]
    pub const fn software() -> Self {
        Self::Software(SoftwareVideoHardwareRequirement {})
    }

    #[must_use]
    pub fn nvidia(encoder: impl Into<String>, decoder: Option<String>) -> Self {
        Self::Nvidia(NvidiaVideoHardwareRequirement {
            encoder: encoder.into(),
            decoder,
        })
    }

    #[must_use]
    pub fn video_toolbox(
        encoder: impl Into<String>,
        decoder: Option<VideoToolboxDecodeRequirement>,
    ) -> Self {
        Self::VideoToolbox(VideoToolboxVideoHardwareRequirement {
            encoder: encoder.into(),
            decoder,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NvidiaVideoHardwareAssignment {
    pub hardware_token: String,
    pub device_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoToolboxVideoHardwareAssignment {
    pub hardware_token: String,
    pub resource_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoHardwareAssignment {
    Software(SoftwareVideoHardwareRequirement),
    Nvidia(NvidiaVideoHardwareAssignment),
    VideoToolbox(VideoToolboxVideoHardwareAssignment),
}

impl VideoHardwareAssignment {
    #[must_use]
    pub const fn software() -> Self {
        Self::Software(SoftwareVideoHardwareRequirement {})
    }

    #[must_use]
    pub fn nvidia(hardware_token: impl Into<String>, device_uuid: impl Into<String>) -> Self {
        Self::Nvidia(NvidiaVideoHardwareAssignment {
            hardware_token: hardware_token.into(),
            device_uuid: device_uuid.into(),
        })
    }

    #[must_use]
    pub fn video_toolbox(
        hardware_token: impl Into<String>,
        resource_id: impl Into<String>,
    ) -> Self {
        Self::VideoToolbox(VideoToolboxVideoHardwareAssignment {
            hardware_token: hardware_token.into(),
            resource_id: resource_id.into(),
        })
    }
}

#[cfg(test)]
#[path = "video_acceleration_test.rs"]
mod tests;
