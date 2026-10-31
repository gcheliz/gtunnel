pub mod certmanager;
pub mod client;
pub mod compute;
pub mod dns;
pub mod gcloud;
pub mod iap;
pub mod monitoring;
pub mod projects;
pub mod retry;

pub use client::{is_not_found, is_precondition_failed, GcpClient, Operation};

// Only tests build a client against an address of their own; production code
// reaches both through `GcpClient::for_account`
#[cfg(test)]
pub use client::{Endpoints, TokenSource};
