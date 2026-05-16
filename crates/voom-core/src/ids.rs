use serde::{Deserialize, Serialize};

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

define_id!(MediaId);
define_id!(TicketId);
define_id!(LeaseId);
define_id!(WorkerId);
define_id!(JobId);
define_id!(EventId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_as_bare_numbers() {
        let id = JobId(42);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "42");
    }

    #[test]
    fn ids_round_trip_through_json() {
        let id = TicketId(7);
        let json = serde_json::to_string(&id).unwrap();
        let back: TicketId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
