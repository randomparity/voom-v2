use voom_worker_protocol::OperationKind;

#[derive(Debug, Clone, Copy)]
pub struct ProviderDefinition {
    pub binary_name: &'static str,
    pub provider: &'static str,
    pub primary: OperationKind,
    pub secondary: &'static [OperationKind],
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ProviderKind {
    Scanner,
    Prober,
    Transcoder,
    Remuxer,
    BackupStore,
    HealthChecker,
    IdentityProvider,
    ExternalSystem,
    QualityScorer,
    IssueProvider,
    UseLeaseProvider,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderCatalogEntry {
    pub(crate) definition: ProviderDefinition,
    pub(crate) kind: ProviderKind,
}

const PROBER_SECONDARY: &[OperationKind] = &[OperationKind::HashFile];
const TRANSCODER_SECONDARY: &[OperationKind] = &[
    OperationKind::TranscodeAudio,
    OperationKind::ExtractAudio,
    OperationKind::TranscribeAudio,
];
const BACKUP_SECONDARY: &[OperationKind] = &[OperationKind::DeleteArtifact];

const PROVIDERS: &[ProviderCatalogEntry] = &[
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-scanner",
            provider: "fake-scanner",
            primary: OperationKind::ScanLibrary,
            secondary: &[],
        },
        kind: ProviderKind::Scanner,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-prober",
            provider: "fake-prober",
            primary: OperationKind::ProbeFile,
            secondary: PROBER_SECONDARY,
        },
        kind: ProviderKind::Prober,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-transcoder",
            provider: "fake-transcoder",
            primary: OperationKind::TranscodeVideo,
            secondary: TRANSCODER_SECONDARY,
        },
        kind: ProviderKind::Transcoder,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-remuxer",
            provider: "fake-remuxer",
            primary: OperationKind::Remux,
            secondary: &[],
        },
        kind: ProviderKind::Remuxer,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-backup-store",
            provider: "fake-backup-store",
            primary: OperationKind::BackUpFile,
            secondary: BACKUP_SECONDARY,
        },
        kind: ProviderKind::BackupStore,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-health-checker",
            provider: "fake-health-checker",
            primary: OperationKind::VerifyArtifact,
            secondary: &[],
        },
        kind: ProviderKind::HealthChecker,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-identity-provider",
            provider: "fake-identity-provider",
            primary: OperationKind::IdentifyMedia,
            secondary: &[],
        },
        kind: ProviderKind::IdentityProvider,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-external-system",
            provider: "fake-external-system",
            primary: OperationKind::SyncExternalSystem,
            secondary: &[],
        },
        kind: ProviderKind::ExternalSystem,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-quality-scorer",
            provider: "fake-quality-scorer",
            primary: OperationKind::ScoreQuality,
            secondary: &[],
        },
        kind: ProviderKind::QualityScorer,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-issue-provider",
            provider: "fake-issue-provider",
            primary: OperationKind::CommitArtifact,
            secondary: &[],
        },
        kind: ProviderKind::IssueProvider,
    },
    ProviderCatalogEntry {
        definition: ProviderDefinition {
            binary_name: "fake-use-lease-provider",
            provider: "fake-use-lease-provider",
            primary: OperationKind::EditTracks,
            secondary: &[],
        },
        kind: ProviderKind::UseLeaseProvider,
    },
];

#[must_use]
pub fn provider_definition(binary_name: &str) -> Option<ProviderDefinition> {
    provider_entry(binary_name).map(|entry| entry.definition)
}

#[must_use]
pub fn provider_definition_for_operation(operation: OperationKind) -> Option<ProviderDefinition> {
    PROVIDERS
        .iter()
        .copied()
        .find(|entry| supports_operation(&entry.definition, operation))
        .map(|entry| entry.definition)
}

pub(crate) fn provider_entry(binary_name: &str) -> Option<ProviderCatalogEntry> {
    PROVIDERS
        .iter()
        .copied()
        .find(|entry| entry.definition.binary_name == binary_name)
}

pub(crate) fn supports_operation(provider: &ProviderDefinition, operation: OperationKind) -> bool {
    provider.primary == operation || provider.secondary.contains(&operation)
}

pub(crate) fn operation_name(operation: OperationKind) -> String {
    serde_json::to_value(operation)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{operation:?}"))
}
