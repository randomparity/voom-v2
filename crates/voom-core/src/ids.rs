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
#[path = "ids_test.rs"]
mod tests;
