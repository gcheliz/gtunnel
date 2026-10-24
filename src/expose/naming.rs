// GCP resource names are at most 63 characters and may hold only lowercase
// letters, digits and dashes
const MAX_NAME: usize = 63;
// "gt-" + the longest account name + "-" + "-auth"
const MAX_DOMAIN_SLUG: usize = 38;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    pub account: String,
    pub relay_vm: String,
    pub instance_group: String,
    pub health_check: String,
    pub default_backend: String,
    pub url_map: String,
    pub https_proxy: String,
    pub address: String,
    pub forwarding_rule: String,
    pub fw_iap_ssh: String,
    pub fw_lb_hc: String,
    pub cert_map: String,
}

// Trim trailing dashes so a truncated name is still a legal GCP name
fn trim(name: String, max: usize) -> String {
    let cut: String = name.chars().take(max).collect();
    cut.trim_end_matches('-').to_string()
}

/// The public hostname of an Expose tunnel. One rule, because creating a
/// tunnel and renaming one must agree on it or a rename drifts the hostname
/// away from the name the backend and DNS record are keyed to.
pub fn hostname_for(tunnel: &str, domain: &str) -> String {
    format!("{tunnel}.{domain}")
}

impl Names {
    pub fn new(account: &str) -> Self {
        let a = account;
        Self {
            account: a.to_string(),
            relay_vm: format!("gt-{a}-relay"),
            instance_group: format!("gt-{a}-ig"),
            health_check: format!("gt-{a}-hc"),
            default_backend: format!("gt-{a}-default"),
            url_map: format!("gt-{a}-urlmap"),
            https_proxy: format!("gt-{a}-https"),
            address: format!("gt-{a}-ip"),
            forwarding_rule: format!("gt-{a}-fr"),
            fw_iap_ssh: format!("gt-{a}-iap-ssh"),
            fw_lb_hc: format!("gt-{a}-lb-hc"),
            cert_map: format!("gt-{a}-certs"),
        }
    }

    // The backend service that serves one tunnel
    // Written into every resource gtunnel creates that lives in a shared
    // namespace, so a later cleanup deletes only its own
    pub fn owner(&self) -> String {
        format!("gtunnel:{}", self.account)
    }

    pub fn backend(&self, tunnel: &str) -> String {
        trim(format!("gt-{}-{tunnel}", self.account), MAX_NAME)
    }

    // The instance-group named port the backend reads its port from
    pub fn named_port(&self, tunnel: &str) -> String {
        trim(format!("gt-{tunnel}"), MAX_NAME)
    }

    pub fn default_named_port(&self) -> String {
        "gt-default".to_string()
    }

    pub fn domain_slug(domain: &str) -> String {
        let slug: String = domain
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        trim(slug.trim_matches('-').to_string(), MAX_DOMAIN_SLUG)
    }

    pub fn certificate(&self, domain: &str) -> String {
        trim(
            format!("gt-{}-{}", self.account, Self::domain_slug(domain)),
            MAX_NAME,
        )
    }

    pub fn dns_auth(&self, domain: &str) -> String {
        trim(
            format!("gt-{}-{}-auth", self.account, Self::domain_slug(domain)),
            MAX_NAME,
        )
    }

    // One entry for the wildcard and one for the apex, as the cert covers both
    pub fn cert_map_entry(&self, domain: &str, wildcard: bool) -> String {
        let suffix = if wildcard { "wild" } else { "apex" };
        trim(
            format!("gt-{}-{suffix}", Self::domain_slug(domain)),
            MAX_NAME,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_and_named_port_follow_the_account_and_tunnel() {
        let n = Names::new("dev");
        assert_eq!(n.backend("api"), "gt-dev-api");
        assert_eq!(n.named_port("api"), "gt-api");
        assert_eq!(n.relay_vm, "gt-dev-relay");
    }

    #[test]
    fn domain_slug_replaces_dots() {
        assert_eq!(Names::domain_slug("dev.example.com"), "dev-example-com");
    }

    // GCP resource names are capped at 63 characters, and the longest account
    // and tunnel names the validators accept must still fit
    #[test]
    fn every_name_fits_in_63_characters() {
        let account = "a".repeat(16);
        let tunnel = "t".repeat(40);
        let domain = format!("{}.example.com", "d".repeat(60));
        let n = Names::new(&account);
        let names = vec![
            n.relay_vm.clone(),
            n.instance_group.clone(),
            n.health_check.clone(),
            n.default_backend.clone(),
            n.url_map.clone(),
            n.https_proxy.clone(),
            n.address.clone(),
            n.forwarding_rule.clone(),
            n.fw_iap_ssh.clone(),
            n.fw_lb_hc.clone(),
            n.cert_map.clone(),
            n.backend(&tunnel),
            n.named_port(&tunnel),
            n.certificate(&domain),
            n.dns_auth(&domain),
            n.cert_map_entry(&domain, true),
            n.cert_map_entry(&domain, false),
        ];
        for name in names {
            assert!(name.len() <= 63, "{name} is {} characters", name.len());
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{name} has characters GCP rejects"
            );
            assert!(!name.ends_with('-'), "{name} ends with a dash");
        }
    }

    #[test]
    fn wildcard_and_apex_entries_differ() {
        let n = Names::new("dev");
        assert_ne!(
            n.cert_map_entry("dev.example.com", true),
            n.cert_map_entry("dev.example.com", false)
        );
    }
}
