//! Typed video-accelerator capability, requirement, and assignment vocabulary.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

/// Per-process deadline for one `VideoToolbox` preflight stage.
pub const VIDEOTOOLBOX_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum sequential process stages in the `VideoToolbox` preflight graph.
pub const VIDEOTOOLBOX_PREFLIGHT_MAX_STAGES: u64 = 4 + 4 + 3 + 5 + 5 + 3 + 5;
/// Allowance for process coordination outside the sequential stage deadlines.
pub const VIDEOTOOLBOX_PREFLIGHT_COORDINATION_SECONDS: u64 = 30;
/// Supervisor deadline covering the complete `VideoToolbox` preflight graph.
pub const VIDEOTOOLBOX_PREFLIGHT_BUDGET: Duration = Duration::from_secs(
    VIDEOTOOLBOX_PREFLIGHT_MAX_STAGES * VIDEOTOOLBOX_PROBE_TIMEOUT.as_secs()
        + VIDEOTOOLBOX_PREFLIGHT_COORDINATION_SECONDS,
);

/// Worker-side deadline covering the complete VAAPI preflight graph (ADR 0052 §7).
pub const VAAPI_READINESS_DEADLINE: Duration = Duration::from_mins(5);
/// Allowance for process coordination outside the worker's readiness deadline.
pub const VAAPI_PREFLIGHT_COORDINATION_SECONDS: u64 = 30;
/// Supervisor deadline covering the complete VAAPI preflight graph.
///
/// Strictly greater than [`VAAPI_READINESS_DEADLINE`], and that ordering is the
/// point. The supervisor starts timing when the child is spawned; the worker starts
/// timing inside its own preflight, after process start and binary resolution, so
/// the supervisor's elapsed time always exceeds the worker's. Were the two deadlines
/// equal, the supervisor would abandon the child first and report a generic
/// "timed out waiting for local worker bound address", and the worker's expiry —
/// which names the stage that did not prove — could never be observed.
pub const VAAPI_PREFLIGHT_BUDGET: Duration =
    Duration::from_secs(VAAPI_READINESS_DEADLINE.as_secs() + VAAPI_PREFLIGHT_COORDINATION_SECONDS);

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
/// address behind them cannot (ADR 0052 §2). `encoders` and `decoders` list only
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

/// Accelerator a local worker bound itself to, discriminated by `backend`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum VideoAcceleratorDescriptor {
    Nvidia(NvidiaVideoAcceleratorDescriptor),
    Vaapi(VaapiVideoAcceleratorDescriptor),
    VideoToolbox(VideoToolboxVideoAcceleratorDescriptor),
}

impl From<NvidiaVideoAcceleratorDescriptor> for VideoAcceleratorDescriptor {
    fn from(value: NvidiaVideoAcceleratorDescriptor) -> Self {
        Self::Nvidia(value)
    }
}

impl From<VaapiVideoAcceleratorDescriptor> for VideoAcceleratorDescriptor {
    fn from(value: VaapiVideoAcceleratorDescriptor) -> Self {
        Self::Vaapi(value)
    }
}

impl From<VideoToolboxVideoAcceleratorDescriptor> for VideoAcceleratorDescriptor {
    fn from(value: VideoToolboxVideoAcceleratorDescriptor) -> Self {
        Self::VideoToolbox(value)
    }
}

impl VideoAcceleratorDescriptor {
    /// The stable device token the scheduler leases and counts capacity against.
    ///
    /// NVIDIA and `VideoToolbox` store the token because their durable rows carry
    /// it; VAAPI derives it from the PCI address, which *is* the identity (ADR 0052
    /// §1) — storing it as well would let the two disagree. Returning an owned
    /// `String` is what lets one accessor serve all three.
    #[must_use]
    pub fn hardware_token(&self) -> String {
        match self {
            Self::Nvidia(value) => value.hardware_token.clone(),
            Self::Vaapi(value) => vaapi_hardware_token(&value.pci_address),
            Self::VideoToolbox(value) => value.hardware_token.clone(),
        }
    }

    #[must_use]
    pub const fn max_sessions(&self) -> u32 {
        match self {
            Self::Nvidia(value) => value.max_sessions,
            Self::Vaapi(value) => value.max_sessions,
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
pub struct VaapiVideoHardwareRequirement {
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
    Vaapi(VaapiVideoHardwareRequirement),
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
    pub fn vaapi(encoder: impl Into<String>, decoder: Option<String>) -> Self {
        Self::Vaapi(VaapiVideoHardwareRequirement {
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

/// Device a VAAPI transcode was assigned to.
///
/// Carries the PCI address alongside the token so a worker can verify the
/// assignment names the device it actually bound (ADR 0052 §1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaapiVideoHardwareAssignment {
    pub hardware_token: String,
    pub pci_address: String,
}

/// The hardware token naming the VAAPI device at `pci_address`.
///
/// The token is derived from the PCI address rather than stored on the descriptor,
/// because the address is the identity and a render-node number is not (ADR 0052
/// §1). Both the scheduler that leases the device and the worker that verifies an
/// assignment against the device it bound must spell it identically, so the
/// derivation lives here with the assignment type rather than at each call site.
#[must_use]
/// **Host-scoped, unlike its siblings.** An NVIDIA token carries a globally unique
/// GPU UUID and a `VideoToolbox` token a hash of the host's platform UUID, but a PCI
/// address is only unique *within* a machine — `0000:03:00.0` is an ordinary slot,
/// so two Linux hosts can each hold a different device behind the same token.
///
/// Everything keyed on the token therefore assumes one Linux host per control
/// plane: accelerator capacity groups on it, and `recover_linux_claim` reads a
/// differing boot id as proof the previous owner's processes are gone. Under two
/// hosts sharing a control plane, capacity for two physically distinct devices
/// would be pooled as one and either host could reclaim the other's live claim.
///
/// Qualifying the token with a boot-invariant host identity (`/etc/machine-id`) is
/// what would lift the assumption; until then it is a real precondition, recorded
/// here and in ADR 0052 §1 rather than left implicit.
pub fn vaapi_hardware_token(pci_address: &str) -> String {
    format!("vaapi:pci-{pci_address}")
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
    Vaapi(VaapiVideoHardwareAssignment),
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
    pub fn vaapi(hardware_token: impl Into<String>, pci_address: impl Into<String>) -> Self {
        Self::Vaapi(VaapiVideoHardwareAssignment {
            hardware_token: hardware_token.into(),
            pci_address: pci_address.into(),
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
