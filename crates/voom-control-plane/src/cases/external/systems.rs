//! External-system registration, health probe, path-mapping CRUD, and link
//! primitives.

use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{ExternalPathMappingId, ExternalSystemId, ExternalSystemLinkId, VoomError};
use voom_events::payload::{
    ExternalSystemHealthChangedPayload, ExternalSystemLinkedPayload,
    ExternalSystemRegisteredPayload, ExternalSystemUnlinkedPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::external::links::{ExternalSystemLink, NewExternalLink};
use voom_store::repo::external::path_mappings::{
    ExternalPathMapping, NewExternalPathMapping, PathMappingUpdate,
};
use voom_store::repo::external::systems::{
    ExternalSystem, ExternalSystemHealth, ExternalSystemKind, NewExternalSystem,
};

use crate::ControlPlane;

use super::super::{append_event, begin_immediate_tx, begin_tx, commit_tx};

impl ControlPlane {
    /// Register an external system (health starts `Unknown`) and emit
    /// `external_system.registered` in the same transaction.
    ///
    /// # Errors
    /// Propagates repository and event-append errors.
    pub async fn register_external_system(
        &self,
        input: NewExternalSystem,
    ) -> Result<ExternalSystem, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let system = self
            .external_systems
            .register_in_tx(&mut tx, input, now)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::ExternalSystem,
            Some(system.id.0),
            now,
            Event::ExternalSystemRegistered(ExternalSystemRegisteredPayload {
                external_system_id: system.id.0,
                kind: system.kind.as_str().to_owned(),
                display_name: system.display_name.clone(),
                health_status: system.health_status.as_str().to_owned(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(system)
    }

    /// Fetch an external system by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_external_system(
        &self,
        id: ExternalSystemId,
    ) -> Result<Option<ExternalSystem>, VoomError> {
        self.external_systems.get(id).await
    }

    /// List active external systems in id order.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_external_systems(&self) -> Result<Vec<ExternalSystem>, VoomError> {
        self.external_systems.list().await
    }

    /// Probe an external system's health and record the result, emitting
    /// `external_system.health_changed` only when the status actually changes.
    ///
    /// # Errors
    /// Returns `NotFound` for an unknown system; propagates probe, repository,
    /// and event-append errors.
    pub async fn health_check_external_system(
        &self,
        id: ExternalSystemId,
    ) -> Result<ExternalSystem, VoomError> {
        let system = self
            .external_systems
            .get(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("external system id={id} not found")))?;
        let probed = self.probe_health(&system).await?;
        let now = self.clock().now();
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let updated = self
            .record_probed_health_in_tx(&mut tx, id, probed, now)
            .await?;
        commit_tx(tx).await?;
        Ok(updated)
    }

    /// Record a probed health status inside the caller's transaction, emitting
    /// `external_system.health_changed` only when the status actually changes.
    /// Shared by `health-check` and `sync` so a sync run's health update and its
    /// `synced` event commit atomically.
    ///
    /// # Errors
    /// Returns `NotFound` for an unknown system; propagates repository and
    /// event-append errors.
    pub(crate) async fn record_probed_health_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: ExternalSystemId,
        probed: ExternalSystemHealth,
        now: OffsetDateTime,
    ) -> Result<ExternalSystem, VoomError> {
        let current = self
            .external_systems
            .get_in_tx(tx, id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("external system id={id} not found")))?;
        let updated = self
            .external_systems
            .set_health_in_tx(tx, id, probed)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("external system id={id} not found")))?;
        if current.health_status != probed {
            append_event(
                &self.events,
                tx,
                SubjectType::ExternalSystem,
                Some(id.0),
                now,
                Event::ExternalSystemHealthChanged(ExternalSystemHealthChangedPayload {
                    external_system_id: id.0,
                    previous: current.health_status.as_str().to_owned(),
                    current: probed.as_str().to_owned(),
                }),
            )
            .await?;
        }
        Ok(updated)
    }

    /// Read-only health probe. V1 probes filesystem-kind systems by checking
    /// their active path mappings' external prefixes; every other kind is
    /// recorded `Unknown` until its provider ships (ADR 0029).
    pub(crate) async fn probe_health(
        &self,
        system: &ExternalSystem,
    ) -> Result<ExternalSystemHealth, VoomError> {
        if system.kind != ExternalSystemKind::Filesystem {
            return Ok(ExternalSystemHealth::Unknown);
        }
        let mappings = self.external_systems.list_path_mappings(system.id).await?;
        if mappings.is_empty() {
            return Ok(ExternalSystemHealth::Unknown);
        }
        let mut present = 0usize;
        for mapping in &mappings {
            let is_dir = tokio::fs::metadata(&mapping.external_prefix)
                .await
                .is_ok_and(|meta| meta.is_dir());
            if is_dir {
                present += 1;
            }
        }
        Ok(if present == 0 {
            ExternalSystemHealth::Unreachable
        } else if present == mappings.len() {
            ExternalSystemHealth::Healthy
        } else {
            ExternalSystemHealth::Degraded
        })
    }

    /// Create a path mapping for a system. No event — path mappings are config.
    ///
    /// # Errors
    /// Returns `NotFound` for an unknown system; propagates repository errors.
    pub async fn create_external_path_mapping(
        &self,
        input: NewExternalPathMapping,
    ) -> Result<ExternalPathMapping, VoomError> {
        self.external_systems
            .create_path_mapping(input, self.clock().now())
            .await
    }

    /// Fetch a path mapping by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_external_path_mapping(
        &self,
        id: ExternalPathMappingId,
    ) -> Result<Option<ExternalPathMapping>, VoomError> {
        self.external_systems.get_path_mapping(id).await
    }

    /// List active path mappings for a system.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_external_path_mappings(
        &self,
        system_id: ExternalSystemId,
    ) -> Result<Vec<ExternalPathMapping>, VoomError> {
        self.external_systems.list_path_mappings(system_id).await
    }

    /// Apply a partial update to a path mapping.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn update_external_path_mapping(
        &self,
        id: ExternalPathMappingId,
        update: PathMappingUpdate,
    ) -> Result<Option<ExternalPathMapping>, VoomError> {
        self.external_systems.update_path_mapping(id, update).await
    }

    /// Retire (soft-delete) a path mapping. Returns whether a row was retired.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn delete_external_path_mapping(
        &self,
        id: ExternalPathMappingId,
    ) -> Result<bool, VoomError> {
        self.external_systems
            .retire_path_mapping(id, self.clock().now())
            .await
    }

    /// Record an external→internal link and emit `external_system.linked`. The
    /// durable primitive a sync reconciles.
    ///
    /// # Errors
    /// Propagates repository and event-append errors.
    pub async fn link_external_ref(
        &self,
        input: NewExternalLink,
    ) -> Result<ExternalSystemLink, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let link = self
            .external_systems
            .record_link_in_tx(&mut tx, input, now)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::ExternalSystem,
            Some(link.external_system_id.0),
            now,
            Event::ExternalSystemLinked(ExternalSystemLinkedPayload {
                external_system_id: link.external_system_id.0,
                link_id: link.id.0,
                target_type: link.target_type.as_str().to_owned(),
                target_id: link.target_id,
                external_ref: link.external_ref.clone(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(link)
    }

    /// Retire a link and emit `external_system.unlinked`. Returns `None` when no
    /// active link has that id.
    ///
    /// # Errors
    /// Propagates repository and event-append errors.
    pub async fn unlink_external_ref(
        &self,
        id: ExternalSystemLinkId,
    ) -> Result<Option<ExternalSystemLink>, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let Some(link) = self
            .external_systems
            .retire_link_in_tx(&mut tx, id, now)
            .await?
        else {
            commit_tx(tx).await?;
            return Ok(None);
        };
        append_event(
            &self.events,
            &mut tx,
            SubjectType::ExternalSystem,
            Some(link.external_system_id.0),
            now,
            Event::ExternalSystemUnlinked(ExternalSystemUnlinkedPayload {
                external_system_id: link.external_system_id.0,
                link_id: link.id.0,
                target_type: link.target_type.as_str().to_owned(),
                target_id: link.target_id,
                external_ref: link.external_ref.clone(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(Some(link))
    }
}

#[cfg(test)]
#[path = "systems_test.rs"]
mod tests;
