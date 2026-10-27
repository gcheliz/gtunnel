// What kind of failure an error is, so a script or an agent can branch on it
// without parsing prose. The kind travels inside the `anyhow` chain as a
// typed error, attached where the failure is created: only the producer
// knows whether "not found" meant a typo or a missing credential, and
// guessing from the message text later would break on the first reworded
// sentence. Anything nobody classified is `Internal`, which is the honest
// answer for a bug or an I/O fault.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Usage,
    NotFound,
    Auth,
    Gcloud,
    Internal,
}

impl ErrorKind {
    /// The string a JSON consumer matches on. Part of the public interface,
    /// shared by `--json` errors and MCP tool errors.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Usage => "usage",
            ErrorKind::NotFound => "not_found",
            ErrorKind::Auth => "auth",
            ErrorKind::Gcloud => "gcloud",
            ErrorKind::Internal => "internal",
        }
    }

    /// 2 is what clap already exits with for a bad command line, so a usage
    /// error means the same thing whoever detected it. 1 stays "internal"
    /// because it is what every failure returned before kinds existed.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorKind::Internal => 1,
            ErrorKind::Usage => 2,
            ErrorKind::NotFound => 3,
            ErrorKind::Auth => 4,
            ErrorKind::Gcloud => 5,
        }
    }

    /// The inverse of `exit_code`, for a parent that only sees how a child
    /// gtunnel ended. A child killed by a signal has no code, and a code
    /// outside the table (a Rust panic is 101) is a bug, so both are
    /// `Internal`.
    pub fn from_exit_code(code: Option<i32>) -> Self {
        [
            ErrorKind::Usage,
            ErrorKind::NotFound,
            ErrorKind::Auth,
            ErrorKind::Gcloud,
        ]
        .into_iter()
        .find(|kind| Some(kind.exit_code()) == code)
        .unwrap_or(ErrorKind::Internal)
    }
}

#[derive(Debug)]
pub struct GtunnelError {
    pub kind: ErrorKind,
    message: String,
}

impl GtunnelError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Usage, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Auth, message)
    }

    pub fn gcloud(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Gcloud, message)
    }
}

// The message alone: the kind is data beside the text, never a prefix on it,
// so the prose a person reads stays what it was before kinds existed
impl fmt::Display for GtunnelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GtunnelError {}

/// The outermost classified cause wins: a context added on top of a gcloud
/// failure by a caller that knows better (a login hint) is reached first.
pub fn kind_of(err: &anyhow::Error) -> ErrorKind {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<GtunnelError>())
        .map_or(ErrorKind::Internal, |e| e.kind)
}

/// The whole chain, as `{e:#}` prints it.
pub fn message_of(err: &anyhow::Error) -> String {
    format!("{err:#}")
}

pub fn to_json(err: &anyhow::Error) -> serde_json::Value {
    serde_json::json!({
        "error": { "kind": kind_of(err).as_str(), "message": message_of(err) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn an_unclassified_error_is_internal() {
        let err = anyhow::anyhow!("disk full");
        assert_eq!(kind_of(&err), ErrorKind::Internal);
        assert_eq!(kind_of(&err).exit_code(), 1);
    }

    #[test]
    fn a_typed_error_keeps_its_kind_under_added_context() {
        let err: anyhow::Error = GtunnelError::not_found("Tunnel 'x' not found").into();
        let err = err.context("while stopping");
        assert_eq!(kind_of(&err), ErrorKind::NotFound);
    }

    #[test]
    fn a_kind_survives_with_context_on_a_result() {
        let r: Result<(), GtunnelError> = Err(GtunnelError::gcloud("gcloud failed"));
        let err = r.context("outer").unwrap_err();
        assert_eq!(kind_of(&err), ErrorKind::Gcloud);
    }

    #[test]
    fn the_message_is_the_whole_chain_joined_with_a_colon() {
        let err: anyhow::Error = GtunnelError::auth("not logged in").into();
        let err = err.context("could not start");
        assert_eq!(message_of(&err), "could not start: not logged in");
    }

    #[test]
    fn display_shows_the_message_without_the_kind() {
        assert_eq!(GtunnelError::usage("bad port").to_string(), "bad port");
    }

    #[test]
    fn every_kind_has_its_own_string_and_exit_code() {
        let kinds = [
            (ErrorKind::Internal, "internal", 1),
            (ErrorKind::Usage, "usage", 2),
            (ErrorKind::NotFound, "not_found", 3),
            (ErrorKind::Auth, "auth", 4),
            (ErrorKind::Gcloud, "gcloud", 5),
        ];
        for (kind, name, code) in kinds {
            assert_eq!(kind.as_str(), name);
            assert_eq!(kind.exit_code(), code);
        }
    }

    #[test]
    fn an_exit_code_maps_back_to_its_kind_and_a_signal_is_internal() {
        for kind in [
            ErrorKind::Internal,
            ErrorKind::Usage,
            ErrorKind::NotFound,
            ErrorKind::Auth,
            ErrorKind::Gcloud,
        ] {
            assert_eq!(ErrorKind::from_exit_code(Some(kind.exit_code())), kind);
        }
        assert_eq!(ErrorKind::from_exit_code(None), ErrorKind::Internal);
        assert_eq!(ErrorKind::from_exit_code(Some(101)), ErrorKind::Internal);
    }

    #[test]
    fn the_json_shape_is_an_error_object_with_kind_and_message() {
        let err: anyhow::Error = GtunnelError::not_found("nope").into();
        assert_eq!(
            to_json(&err),
            serde_json::json!({"error": {"kind": "not_found", "message": "nope"}})
        );
    }
}
