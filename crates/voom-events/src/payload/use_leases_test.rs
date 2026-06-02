use super::*;
use crate::payload::{Event, EventKind};
use time::OffsetDateTime;
#[test]
fn use_lease_acquired_round_trip() {
    let p = UseLeaseAcquiredPayload {
        lease_id: 42,
        kind: "playback".to_owned(),
        scope_type: "asset".to_owned(),
        scope_id: 9,
        issuer_kind: "user".to_owned(),
        issuer_ref: "alice".to_owned(),
        blocking_mode: "blocking".to_owned(),
        ttl_bound: true,
        acquired_at: OffsetDateTime::UNIX_EPOCH,
        expires_at: Some(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(60)),
    };
    let json = serde_json::to_value(Event::UseLeaseAcquired(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseAcquired(q) if q == p));
    assert_eq!(
        Event::UseLeaseAcquired(p).kind(),
        EventKind::UseLeaseAcquired
    );
}

#[test]
fn use_lease_released_round_trip() {
    let p = UseLeaseReleasedPayload {
        lease_id: 7,
        release_reason: "released".to_owned(),
        released_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseReleased(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseReleased(q) if q == p));
    assert_eq!(
        Event::UseLeaseReleased(p).kind(),
        EventKind::UseLeaseReleased
    );
}

#[test]
fn use_lease_expired_round_trip() {
    let p = UseLeaseExpiredPayload {
        lease_id: 7,
        released_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseExpired(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseExpired(q) if q == p));
    assert_eq!(Event::UseLeaseExpired(p).kind(), EventKind::UseLeaseExpired);
}

#[test]
fn use_lease_force_released_round_trip() {
    let p = UseLeaseForceReleasedPayload {
        lease_id: 7,
        actor: "operator-jane".to_owned(),
        reason: "stuck blocking commit".to_owned(),
        released_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseForceReleased(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseForceReleased(q) if q == p));
    assert_eq!(
        Event::UseLeaseForceReleased(p).kind(),
        EventKind::UseLeaseForceReleased
    );
}

#[test]
fn use_lease_recovered_stale_issuer_round_trip() {
    let p = UseLeaseRecoveredStaleIssuerPayload {
        lease_id: 7,
        actor: "operator-jane".to_owned(),
        reason: "worker host gone".to_owned(),
        released_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseRecoveredStaleIssuer(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseRecoveredStaleIssuer(q) if q == p));
    assert_eq!(
        Event::UseLeaseRecoveredStaleIssuer(p).kind(),
        EventKind::UseLeaseRecoveredStaleIssuer
    );
}

#[test]
fn use_lease_reanchored_by_move_round_trip() {
    let p = UseLeaseReanchoredByMovePayload {
        lease_id: 7,
        retired_location_id: 99,
        new_location_id: 100,
        reanchored_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_value(Event::UseLeaseReanchoredByMove(p.clone())).unwrap();
    let back: Event = serde_json::from_value(json).unwrap();
    assert!(matches!(back, Event::UseLeaseReanchoredByMove(q) if q == p));
    assert_eq!(
        Event::UseLeaseReanchoredByMove(p).kind(),
        EventKind::UseLeaseReanchoredByMove
    );
}
