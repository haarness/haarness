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
            site.origin = site_origin(&site.origin)?;
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
        let access = origin
            .and_then(|origin| {
                most_specific(
                    self.sites
                        .iter()
                        .map(|site| (site.origin.as_str(), &site.access)),
                    &origin,
                )
            })
            .unwrap_or(&self.defaults);
        match method {
            "files.upload" | "files.drop" => access.uploads,
            "files.get" => access.downloads,
            _ => access.browsing,
        }
    }
}

/// A site rule's origin in canonical form: an exact HTTP(S) origin, or one
/// with a leading `*.` subdomain wildcard.
fn site_origin(origin: &str) -> CoreResult<String> {
    let wildcard = origin.starts_with("https://*.") || origin.starts_with("http://*.");
    let parseable = if wildcard {
        origin.replacen("*.", "haarness-wildcard.", 1)
    } else {
        origin.to_owned()
    };
    let url = url::Url::parse(&parseable)
        .map_err(|_| CoreError::Invalid("Use a complete http:// or https:// site origin".into()))?;
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
    let origin = url.origin().ascii_serialization();
    Ok(if wildcard {
        origin.replacen("haarness-wildcard.", "*.", 1)
    } else {
        origin
    })
}

/// The most specific rule for `origin`: an exact origin beats a wildcard,
/// and a longer wildcard beats a shorter one.
fn most_specific<'a, T>(rules: impl Iterator<Item = (&'a str, T)>, origin: &str) -> Option<T> {
    rules
        .filter(|(pattern, _)| matches_origin(pattern, origin))
        .max_by_key(|(pattern, _)| (usize::from(!pattern.contains('*')), pattern.len()))
        .map(|(_, value)| value)
}

/// The HTTP(S) origin of a URL or origin, or `None` for anything else
/// (`about:blank`, opaque documents, the tab inventory).
#[must_use]
pub fn web_origin(resource: &str) -> Option<String> {
    let url = url::Url::parse(resource).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.origin().ascii_serialization())
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

/// What Chrome's settings govern. Scripts (`browser_evaluate`) have their own
/// decision, asked even on sites where browsing is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeCategory {
    Browsing,
    Downloads,
    Uploads,
    Scripts,
}
impl ChromeCategory {
    pub const ALL: [Self; 4] = [
        Self::Browsing,
        Self::Downloads,
        Self::Uploads,
        Self::Scripts,
    ];
    /// The categories one browser operation needs.
    #[must_use]
    pub fn for_method(method: &str) -> &'static [Self] {
        match method {
            "files.upload" | "files.drop" => &[Self::Uploads],
            "files.get" => &[Self::Downloads],
            "evaluate" => &[Self::Browsing, Self::Scripts],
            _ => &[Self::Browsing],
        }
    }
    /// The wire name, as in `chrome-<category>:<thread>:<origin>`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Browsing => "browsing",
            Self::Downloads => "downloads",
            Self::Uploads => "uploads",
            Self::Scripts => "scripts",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Browsing => "Browsing",
            Self::Downloads => "Downloads",
            Self::Uploads => "Uploads",
            Self::Scripts => "Scripts",
        }
    }
    const fn index(self) -> usize {
        self as usize
    }
}

fn ask() -> PermissionDecision {
    PermissionDecision::Ask
}
fn yes() -> bool {
    true
}
fn decision_value(value: &serde_json::Value) -> Option<PermissionDecision> {
    match value.as_str()? {
        "allow" => Some(PermissionDecision::Allow),
        "ask" => Some(PermissionDecision::Ask),
        "deny" => Some(PermissionDecision::Deny),
        _ => None,
    }
}
/// A newer client may write a value this server does not know: it reads as
/// Ask, the Chrome default, rather than failing every browser call.
fn lenient_decision<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<PermissionDecision, D::Error> {
    Ok(
        decision_value(&serde_json::Value::deserialize(deserializer)?)
            .unwrap_or(PermissionDecision::Ask),
    )
}
fn lenient_browser<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<DefaultBrowser, D::Error> {
    Ok(
        match serde_json::Value::deserialize(deserializer)?.as_str() {
            Some("haarness") => DefaultBrowser::Haarness,
            Some("chrome") => DefaultBrowser::Chrome,
            _ => DefaultBrowser::Auto,
        },
    )
}
const fn strictness(decision: PermissionDecision) -> u8 {
    match decision {
        PermissionDecision::Allow => 0,
        PermissionDecision::Ask => 1,
        PermissionDecision::Deny => 2,
    }
}
fn stricter(left: PermissionDecision, right: PermissionDecision) -> PermissionDecision {
    if strictness(right) > strictness(left) {
        right
    } else {
        left
    }
}
fn decision_word(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Allow => "Allow",
        PermissionDecision::Ask => "Ask",
        PermissionDecision::Deny => "Block",
    }
}

/// One Chrome decision per category. Unknown values read as Ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeAccess {
    #[serde(default = "ask", deserialize_with = "lenient_decision")]
    pub browsing: PermissionDecision,
    #[serde(default = "ask", deserialize_with = "lenient_decision")]
    pub downloads: PermissionDecision,
    #[serde(default = "ask", deserialize_with = "lenient_decision")]
    pub uploads: PermissionDecision,
    #[serde(default = "ask", deserialize_with = "lenient_decision")]
    pub scripts: PermissionDecision,
}
impl Default for ChromeAccess {
    fn default() -> Self {
        Self {
            browsing: PermissionDecision::Ask,
            downloads: PermissionDecision::Ask,
            uploads: PermissionDecision::Ask,
            scripts: PermissionDecision::Ask,
        }
    }
}
impl ChromeAccess {
    #[must_use]
    pub fn get(&self, category: ChromeCategory) -> PermissionDecision {
        match category {
            ChromeCategory::Browsing => self.browsing,
            ChromeCategory::Downloads => self.downloads,
            ChromeCategory::Uploads => self.uploads,
            ChromeCategory::Scripts => self.scripts,
        }
    }
    fn from_decisions(decisions: [PermissionDecision; 4]) -> Self {
        Self {
            browsing: decisions[ChromeCategory::Browsing.index()],
            downloads: decisions[ChromeCategory::Downloads.index()],
            uploads: decisions[ChromeCategory::Uploads.index()],
            scripts: decisions[ChromeCategory::Scripts.index()],
        }
    }
    fn browser_access(self) -> BrowserAccess {
        BrowserAccess {
            browsing: self.browsing,
            downloads: self.downloads,
            uploads: self.uploads,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeSite {
    pub origin: String,
    #[serde(flatten)]
    pub access: ChromeAccess,
}
/// Which browser `browser_tabs_open` uses when the model names none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DefaultBrowser {
    /// The Haarness browser while the chat is open in Desktop, otherwise Chrome.
    #[default]
    Auto,
    Haarness,
    Chrome,
}
/// `browser.chrome`: the user's own Chrome. Read leniently, so a value a
/// newer client wrote never breaks this server's browser settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub defaults: ChromeAccess,
    #[serde(default, deserialize_with = "lenient_browser")]
    pub default_browser: DefaultBrowser,
    #[serde(default)]
    pub sites: Vec<ChromeSite>,
}
impl Default for ChromeSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            defaults: ChromeAccess::default(),
            default_browser: DefaultBrowser::Auto,
            sites: Vec::new(),
        }
    }
}
impl ChromeSettings {
    /// Stored settings as this server understands them: unknown values read
    /// as defaults, and site rules it cannot use are skipped. `None` when the
    /// value is not settings at all.
    #[must_use]
    pub fn lenient(value: &serde_json::Value) -> Option<Self> {
        let mut settings: Self = match serde_json::from_value(value.clone()) {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(%error, "ignoring unreadable browser.chrome settings");
                return None;
            }
        };
        let mut origins = std::collections::HashSet::new();
        settings
            .sites
            .retain_mut(|site| match site_origin(&site.origin) {
                Ok(origin) if origins.len() < 200 && origins.insert(origin.clone()) => {
                    site.origin = origin;
                    true
                }
                _ => {
                    tracing::warn!(origin = site.origin, "ignoring a Chrome site rule");
                    false
                }
            });
        Some(settings)
    }
    /// Settings a client saves: complete, with known values only.
    pub fn strict(value: &serde_json::Value) -> CoreResult<Self> {
        let invalid = |message: &str| CoreError::Invalid(message.into());
        let complete = |access: &serde_json::Value| {
            ChromeCategory::ALL.iter().all(|category| {
                access
                    .get(category.name())
                    .and_then(decision_value)
                    .is_some()
            })
        };
        let object = value
            .as_object()
            .ok_or_else(|| invalid("Chrome settings must be an object"))?;
        if !object
            .get("enabled")
            .is_some_and(serde_json::Value::is_boolean)
            || !object.get("defaults").is_some_and(complete)
            || !matches!(
                object
                    .get("defaultBrowser")
                    .and_then(serde_json::Value::as_str),
                Some("auto" | "haarness" | "chrome")
            )
            || !object.get("sites").is_some_and(|sites| {
                sites.as_array().is_some_and(|sites| {
                    sites.iter().all(|site| {
                        site.get("origin").is_some_and(serde_json::Value::is_string)
                            && complete(site)
                    })
                })
            })
        {
            return Err(invalid(
                "Save complete Chrome settings: enabled, defaults and sites with browsing, downloads, uploads and scripts, and defaultBrowser auto, haarness or chrome",
            ));
        }
        let mut settings: Self =
            serde_json::from_value(value.clone()).map_err(|error| invalid(&error.to_string()))?;
        settings.validate()?;
        Ok(settings)
    }
    pub fn validate(&mut self) -> CoreResult<()> {
        if self.sites.len() > 200 {
            return Err(CoreError::Invalid(
                "At most 200 Chrome site rules are supported".into(),
            ));
        }
        let mut origins = std::collections::HashSet::new();
        for site in &mut self.sites {
            site.origin = site_origin(&site.origin)?;
            if !origins.insert(site.origin.clone()) {
                return Err(CoreError::Invalid(
                    "A rule for this site already exists".into(),
                ));
            }
        }
        Ok(())
    }
}

/// The policy an extension host enforces: every layer folded into one rule
/// list where the most specific rule wins, exactly as `BrowserPolicy` reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromePolicy {
    pub enabled: bool,
    pub defaults: ChromeAccess,
    pub sites: Vec<ChromeSite>,
}
impl Default for ChromePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            defaults: ChromeAccess::default(),
            sites: Vec::new(),
        }
    }
}
impl ChromePolicy {
    #[must_use]
    pub fn decision(&self, category: ChromeCategory, resource: &str) -> PermissionDecision {
        if !self.enabled {
            return PermissionDecision::Deny;
        }
        web_origin(resource)
            .and_then(|origin| {
                most_specific(
                    self.sites
                        .iter()
                        .map(|site| (site.origin.as_str(), &site.access)),
                    &origin,
                )
            })
            .unwrap_or(&self.defaults)
            .get(category)
    }
    /// The strictest decision across what one browser operation needs.
    #[must_use]
    pub fn method_decision(&self, method: &str, resource: &str) -> PermissionDecision {
        ChromeCategory::for_method(method)
            .iter()
            .map(|category| self.decision(*category, resource))
            .fold(PermissionDecision::Allow, stricter)
    }
    /// The three-category policy Chrome's page filter enforces.
    #[must_use]
    pub fn browser_policy(&self) -> BrowserPolicy {
        BrowserPolicy {
            enabled: self.enabled,
            remember_approvals: true,
            defaults: self.defaults.browser_access(),
            sites: self
                .sites
                .iter()
                .map(|site| BrowserSite {
                    origin: site.origin.clone(),
                    access: site.access.browser_access(),
                })
                .collect(),
        }
    }
}

/// Everything the broker reads from settings for one decision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrowserSettings {
    /// `browser.policy`: the Haarness browser in Desktop.
    pub policy: BrowserPolicy,
    /// The effective policy for the user's Chrome.
    pub chrome: ChromePolicy,
    pub default_browser: DefaultBrowser,
}
impl From<BrowserPolicy> for BrowserSettings {
    fn from(policy: BrowserPolicy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }
}

/// One configuration layer's say over Chrome. `None` leaves a category to
/// less specific rules and other layers; `Allow` means "no tightening".
#[derive(Clone, Debug, Default)]
struct Layer {
    defaults: [Option<PermissionDecision>; 4],
    sites: Vec<(String, [Option<PermissionDecision>; 4])>,
}
impl Layer {
    fn settings(settings: &ChromeSettings) -> Self {
        let all =
            |access: &ChromeAccess| ChromeCategory::ALL.map(|category| Some(access.get(category)));
        Self {
            defaults: all(&settings.defaults),
            sites: settings
                .sites
                .iter()
                .map(|site| (site.origin.clone(), all(&site.access)))
                .collect(),
        }
    }
    /// A project's `browser.chrome`: only what it spells out, to tighten.
    fn project(value: &serde_json::Value) -> Self {
        let decisions = |access: &serde_json::Value| {
            ChromeCategory::ALL.map(|category| access.get(category.name()).and_then(decision_value))
        };
        Self {
            defaults: value.get("defaults").map_or([None; 4], decisions),
            sites: value
                .get("sites")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|site| {
                    let origin = site_origin(site.get("origin")?.as_str()?).ok()?;
                    Some((origin, decisions(site)))
                })
                .collect(),
        }
    }
    /// A layer's shared `browser.policy`: where it blocks, Chrome blocks
    /// too (scripts follow browsing); what it allows or asks says nothing.
    fn shared(policy: &BrowserPolicy) -> Self {
        let blocks = |access: &BrowserAccess| {
            let block = |decision| {
                Some(if decision == PermissionDecision::Deny {
                    PermissionDecision::Deny
                } else {
                    PermissionDecision::Allow
                })
            };
            [
                block(access.browsing),
                block(access.downloads),
                block(access.uploads),
                block(access.browsing),
            ]
        };
        if !policy.enabled {
            return Self {
                defaults: [Some(PermissionDecision::Deny); 4],
                sites: Vec::new(),
            };
        }
        Self {
            defaults: blocks(&policy.defaults),
            sites: policy
                .sites
                .iter()
                .map(|site| (site.origin.clone(), blocks(&site.access)))
                .collect(),
        }
    }
    fn decision(&self, origin: &str, category: ChromeCategory) -> Option<PermissionDecision> {
        most_specific(
            self.sites.iter().filter_map(|(pattern, decisions)| {
                decisions[category.index()].map(|decision| (pattern.as_str(), decision))
            }),
            origin,
        )
        .or(self.defaults[category.index()])
    }
}

/// A concrete origin that only `pattern` and the rules more general than it
/// match, so one evaluation stands for every origin the pattern covers.
fn representative(pattern: &str) -> String {
    pattern.replacen("*.", "haarness-wildcard.", 1)
}

/// Fold layers into one most-specific-wins rule list. Each site any layer
/// names gets the strictest decision every layer gives at that site, so the
/// folded list decides every origin exactly as evaluating each layer would.
fn fold(layers: &[Layer]) -> (ChromeAccess, Vec<ChromeSite>) {
    let decide = |at: Option<&str>| {
        ChromeCategory::ALL.map(|category| {
            layers
                .iter()
                .fold(PermissionDecision::Allow, |decision, layer| {
                    let own = match at {
                        Some(origin) => layer.decision(origin, category),
                        None => layer.defaults[category.index()],
                    };
                    own.map_or(decision, |own| stricter(decision, own))
                })
        })
    };
    let mut origins: Vec<&str> = Vec::new();
    for layer in layers {
        for (origin, _) in &layer.sites {
            if !origins.contains(&origin.as_str()) {
                origins.push(origin);
            }
        }
    }
    let sites = origins
        .into_iter()
        .map(|origin| ChromeSite {
            origin: origin.to_owned(),
            access: ChromeAccess::from_decisions(decide(Some(&representative(origin)))),
        })
        .collect();
    (ChromeAccess::from_decisions(decide(None)), sites)
}

/// Chrome's settings after layering: the global layer decides, a project
/// can only tighten it, and blocks in any layer's shared `browser.policy`
/// (or turning agent browser control off) apply to Chrome too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeLayering {
    /// Global settings tightened by the project, for Settings to show.
    pub settings: ChromeSettings,
    /// What an extension host enforces.
    pub policy: ChromePolicy,
    /// What this project's settings tightened, in words.
    pub restrictions: Vec<String>,
}
impl ChromeLayering {
    /// `global` is the global layer's `browser.chrome`, `projects` each
    /// project layer's, and `shared` every layer's `browser.policy`.
    #[must_use]
    pub fn new(
        global: Option<&serde_json::Value>,
        projects: &[serde_json::Value],
        shared: &[BrowserPolicy],
    ) -> Self {
        let global = global.and_then(ChromeSettings::lenient).unwrap_or_default();
        let global_layer = Layer::settings(&global);
        let project_layers: Vec<Layer> = projects.iter().map(Layer::project).collect();
        let enabled = global.enabled
            && projects
                .iter()
                .all(|project| project.get("enabled") != Some(&serde_json::Value::Bool(false)));
        let mut restrictions = Vec::new();
        if global.enabled && !enabled {
            restrictions.push("Chrome is turned off for this project.".to_owned());
        }
        let mut restrict = |restriction: String| {
            if !restrictions.contains(&restriction) {
                restrictions.push(restriction);
            }
        };
        for layer in &project_layers {
            for category in ChromeCategory::ALL {
                if let Some(decision) = layer.defaults[category.index()]
                    && strictness(decision) > strictness(global.defaults.get(category))
                {
                    restrict(format!(
                        "{}: {} by default",
                        category.label(),
                        decision_word(decision)
                    ));
                }
            }
            for (origin, decisions) in &layer.sites {
                for category in ChromeCategory::ALL {
                    let current = global_layer
                        .decision(&representative(origin), category)
                        .unwrap_or(PermissionDecision::Ask);
                    if let Some(decision) = decisions[category.index()]
                        && strictness(decision) > strictness(current)
                    {
                        restrict(format!(
                            "{} on {origin}: {}",
                            category.label(),
                            decision_word(decision)
                        ));
                    }
                }
            }
        }
        let own: Vec<Layer> = std::iter::once(global_layer.clone())
            .chain(project_layers.iter().cloned())
            .collect();
        let (defaults, sites) = fold(&own);
        let settings = ChromeSettings {
            enabled,
            defaults,
            default_browser: global.default_browser,
            sites,
        };
        let all: Vec<Layer> = own
            .into_iter()
            .chain(shared.iter().map(Layer::shared))
            .collect();
        let (defaults, sites) = fold(&all);
        let policy = ChromePolicy {
            enabled: enabled && shared.iter().all(|policy| policy.enabled),
            defaults,
            sites,
        };
        Self {
            settings,
            policy,
            restrictions,
        }
    }
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
        let denied = BrowserAccess {
            browsing: PermissionDecision::Deny,
            ..BrowserAccess::default()
        };
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

    fn shared(sites: serde_json::Value, browsing: &str) -> BrowserPolicy {
        let mut policy: BrowserPolicy = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "defaults": {"browsing": browsing, "downloads": "allow", "uploads": "allow"},
            "sites": sites,
        }))
        .unwrap();
        policy.validate().unwrap();
        policy
    }

    #[test]
    fn chrome_layers_fold_into_one_list_that_decides_like_every_layer() {
        use PermissionDecision::{Allow, Ask, Deny};
        use serde_json::json;
        let global = json!({"enabled":true,"defaults":{"browsing":"ask","downloads":"ask","uploads":"ask","scripts":"ask"},"defaultBrowser":"chrome","sites":[
            {"origin":"https://*.google.com","browsing":"allow","downloads":"allow","uploads":"allow","scripts":"allow"},
            {"origin":"https://docs.example.com","browsing":"allow","downloads":"allow","uploads":"allow","scripts":"allow"},
        ]});
        // A shared block, exact and by wildcard, applies to Chrome.
        let blocks = shared(
            json!([
                {"origin":"https://mail.google.com","browsing":"deny","downloads":"allow","uploads":"allow"},
                {"origin":"https://*.example.com","browsing":"deny","downloads":"allow","uploads":"deny"},
                {"origin":"https://open.example.org","browsing":"allow","downloads":"allow","uploads":"allow"},
            ]),
            "allow",
        );
        let layering = ChromeLayering::new(Some(&global), &[], &[blocks]);
        let policy = &layering.policy;
        assert_eq!(
            policy.decision(ChromeCategory::Browsing, "https://drive.google.com"),
            Allow
        );
        assert_eq!(
            policy.decision(ChromeCategory::Browsing, "https://mail.google.com/inbox"),
            Deny
        );
        assert_eq!(
            policy.decision(ChromeCategory::Scripts, "https://mail.google.com"),
            Deny
        );
        assert_eq!(
            policy.decision(ChromeCategory::Downloads, "https://mail.google.com"),
            Allow
        );
        // A wildcard block beats a more specific Chrome allow.
        assert_eq!(
            policy.decision(ChromeCategory::Browsing, "https://docs.example.com"),
            Deny
        );
        assert_eq!(
            policy.decision(ChromeCategory::Uploads, "https://a.example.com"),
            Deny
        );
        // Shared allows say nothing about Chrome.
        assert_eq!(
            policy.decision(ChromeCategory::Browsing, "https://open.example.org"),
            Ask
        );
        assert_eq!(
            policy.decision(ChromeCategory::Browsing, "https://google.com"),
            Ask
        );
        // Chrome's own settings for Settings leave the shared blocks out.
        assert_eq!(layering.settings.default_browser, DefaultBrowser::Chrome);
        assert_eq!(layering.settings.sites.len(), 2);
        assert!(layering.restrictions.is_empty());
        // The page filter's three-category policy reads the same.
        let filter = policy.browser_policy();
        assert!(filter.remember_approvals);
        assert_eq!(filter.decision("navigate", "https://mail.google.com"), Deny);
        assert_eq!(
            filter.decision("navigate", "https://drive.google.com"),
            Allow
        );
        assert_eq!(
            filter.decision("files.upload", "https://a.example.com"),
            Deny
        );
        // A shared default block and turning agent browser control off reach Chrome.
        let layering = ChromeLayering::new(Some(&global), &[], &[shared(json!([]), "deny")]);
        assert_eq!(
            layering
                .policy
                .decision(ChromeCategory::Browsing, "https://drive.google.com"),
            Deny
        );
        let off = BrowserPolicy {
            enabled: false,
            ..BrowserPolicy::default()
        };
        let layering = ChromeLayering::new(Some(&global), &[], &[off]);
        assert!(!layering.policy.enabled);
        assert!(layering.settings.enabled);
    }

    #[test]
    fn a_project_only_tightens_chrome() {
        use PermissionDecision::{Allow, Ask, Deny};
        use serde_json::json;
        let global = json!({"enabled":true,"defaults":{"browsing":"allow","downloads":"allow","uploads":"allow","scripts":"ask"},"defaultBrowser":"auto","sites":[
            {"origin":"https://bank.example","browsing":"allow","downloads":"allow","uploads":"allow","scripts":"allow"},
        ]});
        let project = json!({"enabled":true,"defaultBrowser":"chrome","defaults":{"browsing":"allow","scripts":"allow","downloads":"deny"},"sites":[
            {"origin":"https://bank.example","browsing":"ask"},
            {"origin":"https://*.bank.example","uploads":"deny"},
            {"origin":"https://other.example","scripts":"allow"},
        ]});
        let layering = ChromeLayering::new(Some(&global), &[project], &[]);
        let policy = &layering.policy;
        assert_eq!(
            policy.decision(ChromeCategory::Browsing, "https://bank.example"),
            Ask
        );
        assert_eq!(
            policy.decision(ChromeCategory::Scripts, "https://bank.example"),
            Allow
        );
        assert_eq!(
            policy.decision(ChromeCategory::Downloads, "https://bank.example"),
            Deny
        );
        assert_eq!(
            policy.decision(ChromeCategory::Uploads, "https://app.bank.example"),
            Deny
        );
        assert_eq!(
            policy.decision(ChromeCategory::Uploads, "https://bank.example"),
            Allow
        );
        assert_eq!(
            policy.decision(ChromeCategory::Scripts, "https://other.example"),
            Ask
        );
        assert_eq!(
            policy.decision(ChromeCategory::Browsing, "https://news.example"),
            Allow
        );
        assert_eq!(
            layering.settings,
            ChromeSettings {
                enabled: true,
                defaults: policy.defaults,
                default_browser: DefaultBrowser::Auto,
                sites: policy.sites.clone(),
            }
        );
        assert_eq!(
            layering.restrictions,
            vec![
                "Downloads: Block by default",
                "Browsing on https://bank.example: Ask",
                "Uploads on https://*.bank.example: Block",
            ]
        );
        let layering = ChromeLayering::new(Some(&global), &[json!({"enabled":false})], &[]);
        assert!(!layering.policy.enabled && !layering.settings.enabled);
        assert_eq!(
            layering.restrictions,
            vec!["Chrome is turned off for this project."]
        );
        assert_eq!(
            layering
                .policy
                .decision(ChromeCategory::Browsing, "https://news.example"),
            Deny
        );
    }

    #[test]
    fn chrome_settings_read_leniently_and_save_strictly() {
        use serde_json::json;
        let read = ChromeSettings::lenient(&json!({
            "defaults":{"browsing":"allow","scripts":"later"},
            "defaultBrowser":"safari",
            "sites":[{"origin":"https://Example.com:443/","browsing":"deny"},{"origin":"https://example.com"},{"origin":"ftp://example.com"}],
            "unknown": 1,
        }))
        .unwrap();
        assert!(read.enabled);
        assert_eq!(read.defaults.browsing, PermissionDecision::Allow);
        assert_eq!(read.defaults.scripts, PermissionDecision::Ask);
        assert_eq!(read.default_browser, DefaultBrowser::Auto);
        assert_eq!(read.sites.len(), 1);
        assert_eq!(read.sites[0].origin, "https://example.com");
        assert_eq!(read.sites[0].access.downloads, PermissionDecision::Ask);
        assert!(ChromeSettings::lenient(&json!({"sites":"none"})).is_none());
        let complete = json!({"enabled":false,"defaults":{"browsing":"ask","downloads":"ask","uploads":"ask","scripts":"deny"},"defaultBrowser":"haarness","sites":[]});
        let saved = ChromeSettings::strict(&complete).unwrap();
        assert!(!saved.enabled);
        assert_eq!(saved.default_browser, DefaultBrowser::Haarness);
        assert_eq!(serde_json::to_value(&saved).unwrap(), complete);
        for incomplete in [
            json!({"enabled":true}),
            json!({"enabled":true,"defaults":{"browsing":"ask","downloads":"ask","uploads":"ask","scripts":"later"},"defaultBrowser":"auto","sites":[]}),
            json!({"enabled":true,"defaults":{"browsing":"ask","downloads":"ask","uploads":"ask","scripts":"ask"},"defaultBrowser":"safari","sites":[]}),
            json!({"enabled":true,"defaults":{"browsing":"ask","downloads":"ask","uploads":"ask","scripts":"ask"},"defaultBrowser":"auto","sites":[{"origin":"https://example.com","browsing":"ask"}]}),
            json!({"enabled":true,"defaults":{"browsing":"ask","downloads":"ask","uploads":"ask","scripts":"ask"},"defaultBrowser":"auto","sites":[
                {"origin":"https://example.com/path","browsing":"ask","downloads":"ask","uploads":"ask","scripts":"ask"}]}),
        ] {
            assert!(ChromeSettings::strict(&incomplete).is_err(), "{incomplete}");
        }
    }
}
