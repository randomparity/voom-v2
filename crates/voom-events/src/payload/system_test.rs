use super::*;
use crate::payload::Event;
use time::OffsetDateTime;
#[test]
fn schema_initialized_payload_round_trip() {
    let p = SchemaInitializedPayload {
        migrations_applied: 2,
        schema_init_at: OffsetDateTime::UNIX_EPOCH,
    };
    let json = serde_json::to_string(&Event::SchemaInitialized(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::SchemaInitialized(p), back);
}
