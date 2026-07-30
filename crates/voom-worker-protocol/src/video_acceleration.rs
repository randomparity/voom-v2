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

/// Capability of the VAAPI device a worker bound itself to.
///
/// Identity is the PCI address, never a render-node path or ordinal: node
/// numbers are assigned by enumeration order and can renumber, while the
/// address behind them cannot (ADR 0051 §2). `encoders` and `decoders` list only
/// codecs proven by a probe on the bound device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaapiVideoAcceleratorDescriptor {
    pub pci_address: String,
    pub device_name: String,
    pub driver_version: String,
    pub encoders: Vec<String>,
    pub decoders: Vec<String>,
    pub max_sessions: u32,
}

/// Accelerator a local worker bound itself to, discriminated by `backend`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoAcceleratorDescriptor {
    Nvidia(NvidiaVideoAcceleratorDescriptor),
    Vaapi(VaapiVideoAcceleratorDescriptor),
}

impl VideoAcceleratorDescriptor {
    /// The stable device token the scheduler leases and counts capacity against.
    ///
    /// NVIDIA stores the token on the descriptor because pre-#409 rows are durable
    /// and carry it; VAAPI derives it from the PCI address, which is the identity
    /// (ADR 0051 §1). Reading it through one accessor is what lets the scheduler
    /// treat the two backends alike without a per-backend match at every call site.
    #[must_use]
    pub fn hardware_token(&self) -> String {
        match self {
            Self::Nvidia(nvidia) => nvidia.hardware_token.clone(),
            Self::Vaapi(vaapi) => vaapi_hardware_token(&vaapi.pci_address),
        }
    }

    /// Encoders proven usable on the bound device.
    #[must_use]
    pub fn encoders(&self) -> &[String] {
        match self {
            Self::Nvidia(nvidia) => &nvidia.encoders,
            Self::Vaapi(vaapi) => &vaapi.encoders,
        }
    }

    /// Decoders proven usable on the bound device. NVIDIA lists `FFmpeg` decoder
    /// names (`hevc_cuvid`); VAAPI lists source codecs, because `-hwaccel vaapi`
    /// has no per-codec decoder name to carry.
    #[must_use]
    pub fn decoders(&self) -> &[String] {
        match self {
            Self::Nvidia(nvidia) => &nvidia.decoders,
            Self::Vaapi(vaapi) => &vaapi.decoders,
        }
    }

    /// The device's declared and probe-proven concurrent session capacity.
    #[must_use]
    pub const fn max_sessions(&self) -> u32 {
        match self {
            Self::Nvidia(nvidia) => nvidia.max_sessions,
            Self::Vaapi(vaapi) => vaapi.max_sessions,
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
pub struct VaapiVideoHardwareRequirement {
    pub encoder: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoHardwareRequirement {
    Software(SoftwareVideoHardwareRequirement),
    Nvidia(NvidiaVideoHardwareRequirement),
    Vaapi(VaapiVideoHardwareRequirement),
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
    pub fn vaapi(encoder: impl Into<String>, decoder: Option<String>) -> Self {
        Self::Vaapi(VaapiVideoHardwareRequirement {
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

/// Device a VAAPI transcode was assigned to.
///
/// Carries the PCI address alongside the token so a worker can verify the
/// assignment names the device it actually bound (ADR 0051 §1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaapiVideoHardwareAssignment {
    pub hardware_token: String,
    pub pci_address: String,
}

/// The hardware token naming the VAAPI device at `pci_address`.
///
/// The token is derived from the PCI address rather than stored on the descriptor,
/// because the address is the identity and a render-node number is not (ADR 0051
/// §1). Both the scheduler that leases the device and the worker that verifies an
/// assignment against the device it bound must spell it identically, so the
/// derivation lives here with the assignment type rather than at each call site.
#[must_use]
pub fn vaapi_hardware_token(pci_address: &str) -> String {
    format!("vaapi:pci-{pci_address}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoHardwareAssignment {
    Software(SoftwareVideoHardwareRequirement),
    Nvidia(NvidiaVideoHardwareAssignment),
    Vaapi(VaapiVideoHardwareAssignment),
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
    pub fn vaapi(hardware_token: impl Into<String>, pci_address: impl Into<String>) -> Self {
        Self::Vaapi(VaapiVideoHardwareAssignment {
            hardware_token: hardware_token.into(),
            pci_address: pci_address.into(),
        })
    }
}

#[cfg(test)]
#[path = "video_acceleration_test.rs"]
mod tests;
