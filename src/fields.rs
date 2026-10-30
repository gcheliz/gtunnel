//! The words for every value a form asks for, in one place. `init` prints them
//! and the TUI forms show them, so the two cannot describe the same field
//! differently. Flag help in `cli.rs` stays hand-written because clap needs
//! literals, and follows the wording here.

/// One value a form asks for. A variant without text fails to compile
/// (`help` matches exhaustively), and `every_field_has_help` fails if the text
/// is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Configuration,
    AccountName,
    Project,
    Zone,
    Impersonate,
    TunnelName,
    Instance,
    JumpHost,
    RemotePort,
    LocalPort,
    LocalTarget,
    Domain,
    Ports,
}

impl Field {
    #[cfg(test)]
    pub const ALL: [Field; 13] = [
        Field::Configuration,
        Field::AccountName,
        Field::Project,
        Field::Zone,
        Field::Impersonate,
        Field::TunnelName,
        Field::Instance,
        Field::JumpHost,
        Field::RemotePort,
        Field::LocalPort,
        Field::LocalTarget,
        Field::Domain,
        Field::Ports,
    ];

    /// Whether an empty answer is a legitimate answer. This is what separates
    /// `optional, Enter to skip` from a prompt that merely has no default.
    pub fn optional(self) -> bool {
        matches!(self, Field::Zone | Field::Impersonate | Field::JumpHost)
    }
}

/// Said once at the top of `init`, because every later prompt leans on these
/// definitions and a first-time reader has none of them.
pub const INIT_INTRO: &str = "An account is a name gtunnel gives to one GCP project. \
It is built from a gcloud configuration (the profile `gcloud init` made: login, project, zone). \
Enter takes the value shown.";

/// What the value is and the shape it takes. The wording is the one the TUI
/// showed under the prompt, moved here so `init` says the same thing.
pub fn help(f: Field) -> &'static str {
    match f {
        Field::Configuration => {
            "The gcloud configuration whose login, project and zone this account uses"
        }
        Field::AccountName => {
            "What you pass to `--account`. Lower case, digits and dashes, e.g. `dev`"
        }
        Field::Project => "The GCP project every tunnel and API call of this account targets",
        Field::Zone => "Fallback when an instance's zone cannot be looked up, and the relay's zone",
        Field::Impersonate => {
            "Every gcloud call of this account acts as this service account; you need \
             roles/iam.serviceAccountTokenCreator on it. Most people press Enter and act as themselves"
        }
        Field::TunnelName => {
            "What it connects to, never the account: `postgres`, `k8s`. Up to 40 of a-z 0-9 -"
        }
        Field::Instance => {
            "The GCP instance name (port 22 open) IAP connects to. Its port, or a host behind \
             it, becomes local"
        }
        Field::JumpHost => {
            "The private host the GCP instance reaches and forwards to, over ssh. \
             Empty: the GCP instance itself"
        }
        Field::RemotePort => "The port on that GCP instance or host",
        Field::LocalPort => {
            "The port on localhost you connect to. The suggestion is derived from the name"
        }
        Field::LocalTarget => "The service on this machine to publish",
        Field::Domain => {
            "A domain registered with `gtunnel domains add`; the URL is `<name>.<domain>`"
        }
        Field::Ports => {
            "The port on the GCP instance or host, then the localhost port, e.g. 5432:15432"
        }
    }
}

/// The configuration help with its listing command appended, for the TUI's
/// typed configuration field, which has no list above it to read from. A
/// literal because `describe` returns `&'static str`; a test ties it to
/// `help` and `how_to_find` so the three cannot drift.
pub const CONFIGURATION_WITH_LISTING: &str = "The gcloud configuration whose login, project and \
zone this account uses. List them: `gcloud config configurations list`";

/// The command that lists the valid values, for the fields picked from
/// something that already exists. Not `gtunnel zones`: `init` runs before any
/// account does, and that command needs one.
pub fn how_to_find(f: Field) -> Option<&'static str> {
    match f {
        Field::Configuration => Some("gcloud config configurations list"),
        Field::Project => Some("gcloud projects list"),
        Field::Zone => Some("gcloud compute zones list"),
        Field::Instance => Some("gcloud compute instances list"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_has_help() {
        for f in Field::ALL {
            assert!(!help(f).is_empty(), "{f:?} has no help");
            assert!(!help(f).contains("VM"), "{f:?} still says VM");
        }
    }

    #[test]
    fn how_to_find_names_a_gcloud_command_for_listed_values() {
        for f in [Field::Configuration, Field::Project, Field::Zone] {
            assert!(how_to_find(f).unwrap().starts_with("gcloud "), "{f:?}");
        }
    }

    #[test]
    fn the_typed_configuration_text_is_the_help_plus_its_listing_command() {
        assert!(CONFIGURATION_WITH_LISTING.starts_with(help(Field::Configuration)));
        assert!(CONFIGURATION_WITH_LISTING.contains(how_to_find(Field::Configuration).unwrap()));
    }

    #[test]
    fn init_intro_defines_an_account() {
        assert!(INIT_INTRO.contains("gcloud configuration"));
        assert!(INIT_INTRO.contains("GCP project"));
    }
}
