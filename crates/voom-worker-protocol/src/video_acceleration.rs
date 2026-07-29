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
pub struct LocalWorkerBound {
    pub addr: SocketAddr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<NvidiaVideoAcceleratorDescriptor>,
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
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoHardwareRequirement {
    Software(SoftwareVideoHardwareRequirement),
    Nvidia(NvidiaVideoHardwareRequirement),
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NvidiaVideoHardwareAssignment {
    pub hardware_token: String,
    pub device_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoHardwareAssignment {
    Software(SoftwareVideoHardwareRequirement),
    Nvidia(NvidiaVideoHardwareAssignment),
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
}

#[cfg(test)]
#[path = "video_acceleration_test.rs"]
mod tests;
