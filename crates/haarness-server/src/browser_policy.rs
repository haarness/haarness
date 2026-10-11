//! Browser settings are a live constraint in addition to ordinary tool permissions.
use haarness_core::{CoreError, CoreResult, PermissionDecision};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserAccess {
    pub browsing: PermissionDecision,
    pub downloads: PermissionDecision,
    pub uploads: PermissionDecision,
}
impl Default for BrowserAccess {
    fn default() -> Self {
        Self {
            browsing: PermissionDecision::Allow,
            downloads: PermissionDecision::Allow,
            uploads: PermissionDecision::Allow,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserSite {
    pub origin: String,
    #[serde(flatten)]
    pub access: BrowserAccess,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserPolicy {
    pub enabled: bool,
    #[serde(default)]
    pub remember_approvals: bool,
    pub defaults: BrowserAccess,
    pub sites: Vec<BrowserSite>,
}
impl Default for BrowserPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            remember_approvals: false,
            defaults: BrowserAccess::default(),
            sites: Vec::new(),
        }
    }
}
impl BrowserPolicy {
    pub fn validate(&mut self) -> CoreResult<()> {
        if self.sites.len() > 200 {
            return Err(CoreError::Invalid(
                "At most 200 browser site rules are supported".into(),
            ));
        }
        let mut origins = std::collections::HashSet::new();
        for site in &mut self.sites {
            let wildcard =
                site.origin.starts_with("https://*.") || site.origin.starts_with("http://*.");
            let parseable = if wildcard {
                site.origin.replacen("*.", "haarness-wildcard.", 1)
            } else {
                site.origin.clone()
            };
            let url = url::Url::parse(&parseable).map_err(|_| {
                CoreError::Invalid("Use a complete http:// or https:// site origin".into())
            })?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none_or(|host| host.contains('*'))
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.path() != "/"
            {
                return Err(CoreError::Invalid("Site rules use an exact HTTP(S) origin, including its port, without a path or credentials; only a leading *. subdomain wildcard is supported".into()));
            }
            site.origin = url.origin().ascii_serialization();
            if wildcard {
                site.origin = site.origin.replacen("haarness-wildcard.", "*.", 1);
            }
            if !origins.insert(site.origin.clone()) {
                return Err(CoreError::Invalid(
                    "A rule for this site already exists".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn ensure_enabled(&self) -> CoreResult<()> {
        if self.enabled {
            Ok(())
        } else {
            Err(CoreError::PermissionDenied("Agent browser control is disabled in Settings → Browser. Ask the user to enable it; do not retry browser tools.".into()))
        }
    }
    pub fn decision(&self, method: &str, resource: &str) -> PermissionDecision {
        if !self.enabled {
            return PermissionDecision::Deny;
        }
        let origin = url::Url::parse(resource)
            .ok()
            .map(|url| url.origin().ascii_serialization());
        let access = self
            .sites
            .iter()
            .filter(|site| {
                origin
                    .as_ref()
                    .is_some_and(|origin| matches_origin(&site.origin, origin))
            })
            .max_by_key(|site| (usize::from(!site.origin.contains('*')), site.origin.len()))
            .map(|site| &site.access)
            .unwrap_or(&self.defaults);
        match method {
            "files.upload" | "files.drop" => access.uploads,
            "files.get" => access.downloads,
            _ => access.browsing,
        }
    }
}

/// Wildcards include subdomains only and preserve scheme and port boundaries.
fn matches_origin(pattern: &str, origin: &str) -> bool {
    if pattern == origin {
        return true;
    }
    let Some((prefix, suffix)) = pattern.split_once("*.") else {
        return false;
    };
    origin.strip_prefix(prefix).is_some_and(|host| {
        host.strip_suffix(suffix)
            .is_some_and(|subdomain| subdomain.ends_with('.') && subdomain.len() > 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wildcard_subdomains_are_scoped_and_exact_rules_win() {
        let mut policy = BrowserPolicy::default();
        policy.defaults.browsing = PermissionDecision::Deny;
        policy.sites.push(BrowserSite {
            origin: "https://*.example.test:443".into(),
            access: BrowserAccess::default(),
        });
        let mut denied = BrowserAccess::default();
        denied.browsing = PermissionDecision::Deny;
        policy.sites.push(BrowserSite {
            origin: "https://private.example.test".into(),
            access: denied,
        });
        policy.validate().unwrap();
        for url in ["https://a.example.test/path", "https://a.b.example.test"] {
            assert_eq!(policy.decision("navigate", url), PermissionDecision::Allow);
        }
        for url in [
            "https://example.test",
            "https://private.example.test",
            "https://a.example.test.evil",
            "http://a.example.test",
            "https://a.example.test:444",
        ] {
            assert_eq!(
                policy.decision("navigate", url),
                PermissionDecision::Deny,
                "{url}"
            );
        }
    }
    #[test]
    fn rules_match_origins_not_prefixes_and_default_ports_are_normalized() {
        let mut policy = BrowserPolicy::default();
        policy.defaults.browsing = PermissionDecision::Ask;
        policy.sites.push(BrowserSite {
            origin: "https://EXAMPLE.test:443/".into(),
            access: BrowserAccess::default(),
        });
        policy.validate().unwrap();
        assert_eq!(
            policy.decision("navigate", "https://example.test/path?q=1"),
            PermissionDecision::Allow
        );
        for url in [
            "https://example.test.evil/path",
            "http://example.test",
            "https://example.test:444",
        ] {
            assert_eq!(policy.decision("navigate", url), PermissionDecision::Ask);
        }
        policy.enabled = false;
        assert_eq!(
            policy.decision("navigate", "https://example.test"),
            PermissionDecision::Deny
        );
    }
    #[test]
    fn rejects_ambiguous_rules_and_validates_flattened_fields() {
        let mut policy: BrowserPolicy = serde_json::from_value(serde_json::json!({"enabled":true,"defaults":{"browsing":"ask","downloads":"deny","uploads":"allow"},"sites":[{"origin":"http://localhost:3000","browsing":"allow","downloads":"ask","uploads":"deny"}]})).unwrap();
        policy.validate().unwrap();
        assert_eq!(
            policy.decision("files.upload", "http://localhost:3000/form"),
            PermissionDecision::Deny
        );
        for origin in [
            "https://example.test/path",
            "https://user@example.test",
            "https://example.*.test",
            "file:///tmp/foo",
        ] {
            policy.sites[0].origin = origin.into();
            assert!(policy.validate().is_err(), "{origin}");
        }
    }
}
