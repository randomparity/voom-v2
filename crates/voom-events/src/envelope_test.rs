use super::*;

use time::OffsetDateTime;

#[test]
fn envelope_construction_round_trips_payload_via_serde_json() {
    // The envelope itself isn't serialized; only the Event payload is.
    // This test pins the payload serde round-trip — EventRepo persists
    // each envelope field separately and reassembles the Event from the
    // stored kind + payload columns. `kind` is derived from `payload`.
    let env = EventEnvelope {
        occurred_at: OffsetDateTime::UNIX_EPOCH,
        subject_type: SubjectType::System,
        subject_id: None,
        trace_id: None,
        payload: Event::SchemaInitialized(crate::payload::SchemaInitializedPayload {
            migrations_applied: 2,
            schema_init_at: OffsetDateTime::UNIX_EPOCH,
        }),
    };
    let json = serde_json::to_string(&env.payload).expect("payload serializes");
    let back: Event = serde_json::from_str(&json).expect("payload deserializes");
    assert_eq!(env.payload, back);
}

#[test]
fn trace_id_wraps_a_string() {
    // TraceId is a transparent newtype, not a serde target. Field-level
    // persistence of trace_id stores the inner String directly.
    let t = TraceId("abc-123".to_owned());
    assert_eq!(t.0, "abc-123");
}
