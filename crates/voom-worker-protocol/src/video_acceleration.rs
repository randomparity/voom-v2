//! Typed video-accelerator capability, requirement, and assignment vocabulary.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

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

/// Maximum UTF-8 byte length of one public accelerator descriptor string.
pub const MAX_ACCELERATOR_DESCRIPTOR_STRING_BYTES: usize = 256;
/// Maximum entries in any one accelerator descriptor collection.
pub const MAX_ACCELERATOR_DESCRIPTOR_COLLECTION_ITEMS: usize = 64;
/// Maximum JSON-encoded size of one accelerator descriptor. The 3 KiB cap leaves
/// headroom for the address and framing inside the supervisor's 4 KiB readiness line.
pub const MAX_ACCELERATOR_DESCRIPTOR_ENCODED_BYTES: usize = 3 * 1024;

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

    /// Validate invariants required before a descriptor crosses a worker
    /// declaration or readiness boundary.
    ///
    /// # Errors
    ///
    /// Returns an actionable message when device identity or session capacity
    /// cannot participate in scheduling.
    pub fn validate_declaration(&self) -> Result<(), String> {
        match self {
            Self::Nvidia(nvidia) => validate_nvidia_descriptor(nvidia)?,
            Self::Vaapi(vaapi) => validate_vaapi_descriptor(vaapi)?,
            Self::VideoToolbox(videotoolbox) => {
                validate_videotoolbox_descriptor(videotoolbox)?;
            }
        }
        let encoded = serde_json::to_vec(self)
            .map_err(|error| format!("accelerator descriptor encode failed: {error}"))?;
        if encoded.len() > MAX_ACCELERATOR_DESCRIPTOR_ENCODED_BYTES {
            return Err(format!(
                "accelerator descriptor encoded size is {} bytes, above the \
                 {MAX_ACCELERATOR_DESCRIPTOR_ENCODED_BYTES}-byte bound",
                encoded.len()
            ));
        }
        Ok(())
    }
}

fn validate_nvidia_descriptor(nvidia: &NvidiaVideoAcceleratorDescriptor) -> Result<(), String> {
    for (field, value) in [
        ("NVIDIA hardware_token", nvidia.hardware_token.as_str()),
        ("NVIDIA device_uuid", nvidia.device_uuid.as_str()),
        ("NVIDIA device_name", nvidia.device_name.as_str()),
        ("NVIDIA driver_version", nvidia.driver_version.as_str()),
    ] {
        validate_descriptor_string(field, value)?;
    }
    validate_string_collection("NVIDIA encoders", &nvidia.encoders)?;
    validate_string_collection("NVIDIA decoders", &nvidia.decoders)?;
    if !is_full_nvidia_uuid(&nvidia.device_uuid) {
        return Err("NVIDIA device_uuid must be a full GPU- UUID".to_owned());
    }
    if nvidia.hardware_token.strip_prefix("nvidia:") != Some(nvidia.device_uuid.as_str()) {
        return Err("NVIDIA hardware_token must equal `nvidia:<device_uuid>`".to_owned());
    }
    validate_session_capacity("NVIDIA", nvidia.max_sessions)
}

fn validate_vaapi_descriptor(vaapi: &VaapiVideoAcceleratorDescriptor) -> Result<(), String> {
    for (field, value) in [
        ("VAAPI pci_address", vaapi.pci_address.as_str()),
        ("VAAPI device_name", vaapi.device_name.as_str()),
        ("VAAPI driver_version", vaapi.driver_version.as_str()),
    ] {
        validate_descriptor_string(field, value)?;
    }
    validate_string_collection("VAAPI encoders", &vaapi.encoders)?;
    validate_string_collection("VAAPI decoders", &vaapi.decoders)?;
    if !is_pci_address(&vaapi.pci_address) {
        return Err(
            "VAAPI pci_address must be lowercase `dddd:bb:dd.f`, not a render-node path or ordinal"
                .to_owned(),
        );
    }
    validate_session_capacity("VAAPI", vaapi.max_sessions)
}

fn validate_videotoolbox_descriptor(
    videotoolbox: &VideoToolboxVideoAcceleratorDescriptor,
) -> Result<(), String> {
    for (field, value) in [
        (
            "VideoToolbox hardware_token",
            videotoolbox.hardware_token.as_str(),
        ),
        (
            "VideoToolbox resource_id",
            videotoolbox.resource_id.as_str(),
        ),
        (
            "VideoToolbox model_identifier",
            videotoolbox.model_identifier.as_str(),
        ),
        ("VideoToolbox chip_name", videotoolbox.chip_name.as_str()),
        (
            "VideoToolbox macos_version",
            videotoolbox.macos_version.as_str(),
        ),
        (
            "VideoToolbox macos_build",
            videotoolbox.macos_build.as_str(),
        ),
    ] {
        validate_descriptor_string(field, value)?;
    }
    validate_string_collection("VideoToolbox encoders", &videotoolbox.encoders)?;
    validate_collection_bound("VideoToolbox decoders", videotoolbox.decoders.len())?;
    let mut decoder_codecs = HashSet::with_capacity(videotoolbox.decoders.len());
    for decoder in &videotoolbox.decoders {
        validate_descriptor_string("VideoToolbox decoder codec", &decoder.codec)?;
        if !decoder_codecs.insert(decoder.codec.as_str()) {
            return Err("VideoToolbox decoders contain a duplicate decoder codec".to_owned());
        }
        validate_string_collection("VideoToolbox decoder pixel_formats", &decoder.pixel_formats)?;
    }
    if videotoolbox.hardware_token.strip_prefix("videotoolbox:")
        != Some(videotoolbox.resource_id.as_str())
    {
        return Err(
            "VideoToolbox hardware_token must equal `videotoolbox:<resource_id>`".to_owned(),
        );
    }
    validate_session_capacity("VideoToolbox", videotoolbox.max_sessions)
}

fn validate_descriptor_string(field: &str, value: &str) -> Result<(), String> {
    if invalid_descriptor_string(value) {
        return Err(format!(
            "{field} must be 1..={MAX_ACCELERATOR_DESCRIPTOR_STRING_BYTES} non-blank UTF-8 bytes \
             without control characters; got {} bytes",
            value.len()
        ));
    }
    Ok(())
}

fn validate_string_collection(field: &str, values: &[String]) -> Result<(), String> {
    validate_collection_bound(field, values.len())?;
    let mut unique = HashSet::with_capacity(values.len());
    for value in values {
        if invalid_descriptor_string(value) {
            return Err(format!(
                "{field} entries must each be 1..={MAX_ACCELERATOR_DESCRIPTOR_STRING_BYTES} \
                 non-blank UTF-8 bytes without control characters; got {} bytes",
                value.len()
            ));
        }
        if !unique.insert(value.as_str()) {
            return Err(format!("{field} contains a duplicate entry"));
        }
    }
    Ok(())
}

fn invalid_descriptor_string(value: &str) -> bool {
    value.trim().is_empty()
        || value.len() > MAX_ACCELERATOR_DESCRIPTOR_STRING_BYTES
        || value.chars().any(char::is_control)
}

fn validate_collection_bound(field: &str, length: usize) -> Result<(), String> {
    if length > MAX_ACCELERATOR_DESCRIPTOR_COLLECTION_ITEMS {
        return Err(format!(
            "{field} contains {length} entries, above the \
             {MAX_ACCELERATOR_DESCRIPTOR_COLLECTION_ITEMS}-entry bound"
        ));
    }
    Ok(())
}

fn validate_session_capacity(backend: &str, max_sessions: u32) -> Result<(), String> {
    if !(1..=16).contains(&max_sessions) {
        return Err(format!("{backend} max_sessions must be in 1..=16"));
    }
    Ok(())
}

fn is_pci_address(pci_address: &str) -> bool {
    let Some((domain, rest)) = pci_address.split_once(':') else {
        return false;
    };
    let Some((bus, device_function)) = rest.split_once(':') else {
        return false;
    };
    let Some((device, function)) = device_function.split_once('.') else {
        return false;
    };
    let hex = |text: &str, width: usize| {
        text.len() == width && text.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    hex(domain, 4)
        && hex(bus, 2)
        && hex(device, 2)
        && hex(function, 1)
        && !pci_address.bytes().any(|byte| byte.is_ascii_uppercase())
}

fn is_full_nvidia_uuid(device_uuid: &str) -> bool {
    let Some(uuid) = device_uuid.strip_prefix("GPU-") else {
        return false;
    };
    uuid.len() == 36
        && uuid.char_indices().all(|(index, character)| match index {
            8 | 13 | 18 | 23 => character == '-',
            _ => character.is_ascii_hexdigit(),
        })
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
