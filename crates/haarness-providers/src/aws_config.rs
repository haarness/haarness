//! The AWS sign-in a person already has for the AWS CLI: keys in the
//! environment, a profile of `~/.aws/credentials` and `~/.aws/config`
//! (static keys, a `credential_process`, or IAM Identity Center and assumed
//! roles, which the AWS CLI resolves), and the region the profile names.

use std::{collections::BTreeMap, path::PathBuf, sync::Mutex, time::Duration};

use chrono::{DateTime, Utc};
use haarness_core::{CoreError, CoreResult};
use secrecy::SecretString;
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct AwsCredentials {
    pub access_key: String,
    pub secret_key: SecretString,
    pub session_token: Option<SecretString>,
}

/// Where SigV4 credentials come from.
#[derive(Clone)]
enum Source {
    /// Keys from the environment or a profile's `aws_access_key_id`.
    Static(AwsCredentials),
    /// A profile's `credential_process`, run as the AWS CLI runs it.
    Process(String),
    /// A profile only the AWS CLI can turn into keys: IAM Identity Center
    /// (`sso_session`, `sso_start_url`), an assumed role (`role_arn`), a web
    /// identity. `aws configure export-credentials` does it.
    Cli,
}

/// The AWS credentials this machine's AWS setup provides, resolved when a
/// request needs them and kept until shortly before they expire.
pub(crate) struct AwsSignIn {
    profile: String,
    /// Whether the person chose the profile (`AWS_PROFILE`) or set keys in
    /// the environment, rather than it being the files' `default`.
    chosen: bool,
    source: Source,
    cached: Mutex<Option<(AwsCredentials, Option<DateTime<Utc>>)>>,
    /// Whether credentials have been had from it since Haarness started.
    worked: std::sync::atomic::AtomicBool,
}

/// How long before their expiry temporary credentials are renewed, and how
/// long credentials that name no expiry are kept before they are read again
/// (a tool that rotates the keys a profile holds).
const RENEW_BEFORE: Duration = Duration::from_secs(5 * 60);
const KEEP_UNDATED: Duration = Duration::from_secs(15 * 60);

impl AwsSignIn {
    /// The sign-in the environment and the shared AWS files describe, or
    /// `None` when there is none.
    pub fn from_environment() -> Option<Self> {
        let profile = profile_name();
        if let (Ok(access_key), Ok(secret_key)) = (
            std::env::var("AWS_ACCESS_KEY_ID"),
            std::env::var("AWS_SECRET_ACCESS_KEY"),
        ) && !access_key.is_empty()
            && !secret_key.is_empty()
        {
            return Some(Self::new(
                profile,
                Source::Static(AwsCredentials {
                    access_key,
                    secret_key: SecretString::from(secret_key),
                    session_token: std::env::var("AWS_SESSION_TOKEN")
                        .ok()
                        .filter(|token| !token.is_empty())
                        .map(SecretString::from),
                }),
            ));
        }
        let credentials = read(credentials_file());
        let config = read(config_file());
        let source = profile_source(&credentials, &config, &profile)?;
        Some(Self::new(profile, source))
    }

    fn new(profile: String, source: Source) -> Self {
        let chosen = matches!(source, Source::Static(_))
            || std::env::var("AWS_PROFILE").is_ok_and(|profile| !profile.is_empty());
        Self {
            profile,
            chosen,
            source,
            cached: Mutex::new(None),
            worked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Whether Bedrock reads as connected through this sign-in: keys at
    /// hand, a profile the person chose, or one that has given credentials.
    /// A `default` profile that signs in through a program counts once it
    /// has worked, so an AWS setup made for other tools, its SSO sign-in
    /// expired, does not make Bedrock a chat's default model.
    pub fn usable(&self) -> bool {
        self.chosen || self.worked.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn fixed(credentials: AwsCredentials) -> Self {
        Self::new("default".into(), Source::Static(credentials))
    }

    /// Keys a profile names itself are at hand without running anything.
    pub fn access_key_hint(&self) -> Option<&str> {
        match &self.source {
            Source::Static(credentials) => Some(&credentials.access_key),
            _ => None,
        }
    }

    /// The credentials to sign a request with.
    pub async fn credentials(&self) -> CoreResult<AwsCredentials> {
        let command = match &self.source {
            Source::Static(credentials) => return Ok(credentials.clone()),
            Source::Process(command) => command.clone(),
            Source::Cli => format!(
                "aws configure export-credentials --profile {} --format process",
                shell_word(&self.profile)
            ),
        };
        if let Some((credentials, expires)) = self.cached.lock().unwrap().as_ref()
            && expires.is_some_and(|expires| {
                Utc::now() + chrono::Duration::from_std(RENEW_BEFORE).unwrap_or_default() < expires
            })
        {
            return Ok(credentials.clone());
        }
        let (credentials, expires) = self.run(&command).await?;
        self.worked
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let kept = expires.or_else(|| {
            Some(Utc::now() + chrono::Duration::from_std(KEEP_UNDATED).unwrap_or_default())
        });
        *self.cached.lock().unwrap() = Some((credentials.clone(), kept));
        Ok(credentials)
    }

    /// Forget credentials Bedrock called expired, so the next request reads
    /// them afresh.
    pub fn forget(&self) {
        *self.cached.lock().unwrap() = None;
    }

    async fn run(&self, command: &str) -> CoreResult<(AwsCredentials, Option<DateTime<Utc>>)> {
        let mut process = if cfg!(windows) {
            let mut process = tokio::process::Command::new("cmd");
            process.args(["/C", command]);
            process
        } else {
            let mut process = tokio::process::Command::new("sh");
            process.args(["-c", command]);
            process
        };
        process
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(60), process.output())
            .await
            .map_err(|_| self.failure("the AWS CLI did not answer within a minute"))?
            .map_err(|error| self.failure(&error.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // `sh: aws: command not found`, or cmd's "is not recognized".
            if matches!(self.source, Source::Cli)
                && (output.status.code() == Some(127)
                    || stderr.contains("is not recognized as an internal or external command"))
            {
                return Err(CoreError::Provider(format!(
                    "Amazon Bedrock could not get AWS credentials for the AWS profile {}: it signs in through the AWS CLI, which isn't installed here. Install the AWS CLI and run `aws sso login --profile {}`, or connect Amazon Bedrock with an API key.",
                    self.profile, self.profile
                )));
            }
            let reason = stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("it exited without saying why");
            return Err(self.failure(reason));
        }
        parse_process_output(&output.stdout)
            .ok_or_else(|| self.failure("its answer held no AccessKeyId and SecretAccessKey"))
    }

    fn failure(&self, reason: &str) -> CoreError {
        let reason = reason.trim_end_matches('.');
        let sign_in = if matches!(self.source, Source::Cli) {
            format!(
                " If the profile signs in through AWS IAM Identity Center, run `aws sso login --profile {}` and try again.",
                self.profile
            )
        } else {
            String::new()
        };
        CoreError::Provider(format!(
            "Amazon Bedrock could not get AWS credentials for the AWS profile {}: {reason}.{sign_in}",
            self.profile
        ))
    }
}

/// The region a request goes to when none is set in the environment: the
/// one the AWS profile names.
pub(crate) fn profile_region() -> Option<String> {
    let profile = profile_name();
    let config = read(config_file());
    section(&config, &profile, true)
        .remove("region")
        .filter(|region| !region.is_empty())
}

/// The region a short-term Bedrock API key was made for: it is a presigned
/// URL (`bedrock-api-key-` and its base64), valid in that region only.
pub(crate) fn api_key_region(key: &str) -> Option<String> {
    use base64::Engine as _;
    let encoded = key.strip_prefix("bedrock-api-key-")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let url = String::from_utf8(decoded).ok()?;
    let (_, query) = url.split_once('?')?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name == "X-Amz-Credential")
        // `AKIA…/20261111/eu-west-1/bedrock/aws4_request`
        .and_then(|(_, scope)| scope.split('/').nth(2).map(str::to_owned))
        .filter(|region| !region.is_empty())
}

/// The region of a Bedrock endpoint (`bedrock-runtime.eu-west-1.amazonaws.com`,
/// and the FIPS and VPC endpoint spellings), which a SigV4 signature must
/// name.
pub(crate) fn endpoint_region(endpoint: &str) -> Option<String> {
    let host = url::Url::parse(endpoint)
        .ok()?
        .host_str()?
        .to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let at = labels
        .iter()
        .position(|label| label.starts_with("bedrock"))?;
    let region = labels.get(at + 1)?;
    (host.ends_with(".amazonaws.com") && region.contains('-')).then(|| (*region).to_owned())
}

fn profile_name() -> String {
    std::env::var("AWS_PROFILE")
        .or_else(|_| std::env::var("AWS_DEFAULT_PROFILE"))
        .ok()
        .filter(|profile| !profile.is_empty())
        .unwrap_or_else(|| "default".into())
}

fn aws_dir() -> Option<PathBuf> {
    haarness_protocol::home_dir().map(|home| home.join(".aws"))
}

fn credentials_file() -> Option<PathBuf> {
    std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
        .map(PathBuf::from)
        .or_else(|| aws_dir().map(|dir| dir.join("credentials")))
}

fn config_file() -> Option<PathBuf> {
    std::env::var_os("AWS_CONFIG_FILE")
        .map(PathBuf::from)
        .or_else(|| aws_dir().map(|dir| dir.join("config")))
}

fn read(path: Option<PathBuf>) -> String {
    path.and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
}

/// The keys of one profile: `[name]` in the credentials file; `[profile
/// name]`, or `[default]`, in the config file. Values continued on indented
/// lines (nested `s3 =` blocks) are left out.
fn section(contents: &str, profile: &str, config: bool) -> BTreeMap<String, String> {
    let wanted = |name: &str| {
        let name = name.trim();
        if config && profile != "default" {
            name.strip_prefix("profile")
                .is_some_and(|rest| rest.starts_with(char::is_whitespace) && rest.trim() == profile)
        } else {
            name == profile
                || (config && name.strip_prefix("profile ").map(str::trim) == Some(profile))
        }
    };
    let mut inside = false;
    let mut values = BTreeMap::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            inside = wanted(name);
            continue;
        }
        if inside
            && !raw.starts_with(char::is_whitespace)
            && let Some((key, value)) = line.split_once('=')
        {
            values.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    values
}

/// What a profile signs in with, as the AWS CLI reads it: static keys from
/// either file (the credentials file first), else a `credential_process`,
/// else a profile the CLI resolves.
fn profile_source(credentials: &str, config: &str, profile: &str) -> Option<Source> {
    let mut keys = section(credentials, profile, false);
    let mut settings = section(config, profile, true);
    for key in [
        "aws_access_key_id",
        "aws_secret_access_key",
        "aws_session_token",
    ] {
        if let Some(value) = settings.remove(key) {
            keys.entry(key.to_owned()).or_insert(value);
        }
    }
    if let (Some(access_key), Some(secret_key)) = (
        keys.remove("aws_access_key_id")
            .filter(|value| !value.is_empty()),
        keys.remove("aws_secret_access_key")
            .filter(|value| !value.is_empty()),
    ) {
        return Some(Source::Static(AwsCredentials {
            access_key,
            secret_key: SecretString::from(secret_key),
            session_token: keys
                .remove("aws_session_token")
                .filter(|value| !value.is_empty())
                .map(SecretString::from),
        }));
    }
    if let Some(command) = keys
        .remove("credential_process")
        .or_else(|| settings.remove("credential_process"))
        .filter(|command| !command.is_empty())
    {
        return Some(Source::Process(command));
    }
    [
        "sso_session",
        "sso_start_url",
        "role_arn",
        "web_identity_token_file",
        "credential_source",
        "login_session",
    ]
    .iter()
    .any(|key| settings.contains_key(*key))
    .then_some(Source::Cli)
}

/// The JSON a `credential_process` prints (`Version`, `AccessKeyId`,
/// `SecretAccessKey`, `SessionToken`, `Expiration`).
fn parse_process_output(stdout: &[u8]) -> Option<(AwsCredentials, Option<DateTime<Utc>>)> {
    let value: Value = serde_json::from_slice(stdout).ok()?;
    let text = |key: &str| {
        value[key]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let credentials = AwsCredentials {
        access_key: text("AccessKeyId")?,
        secret_key: SecretString::from(text("SecretAccessKey")?),
        session_token: text("SessionToken").map(SecretString::from),
    };
    let expires = text("Expiration")
        .and_then(|time| DateTime::parse_from_rfc3339(&time).ok())
        .map(|time| time.with_timezone(&Utc));
    Some((credentials, expires))
}

/// A profile name as one shell word.
fn shell_word(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.@+=,/:".contains(&byte))
    {
        value.to_owned()
    } else if cfg!(windows) {
        format!("\"{}\"", value.replace('"', ""))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret as _;

    #[test]
    fn a_profile_signs_in_as_the_aws_cli_reads_it() {
        let credentials = "[default]\naws_access_key_id = AKIDDEFAULT\naws_secret_access_key = secret\n\n[work]\naws_access_key_id=AKIDWORK\naws_secret_access_key=worksecret\naws_session_token=token\n";
        let config = "[default]\nregion = eu-west-1\n\n[profile keys-in-config]\naws_access_key_id = AKIDCONFIG\naws_secret_access_key = configsecret\n\n[profile sso]\nsso_session = corp\nsso_account_id = 123456789012\nsso_role_name = Dev\nregion = eu-central-1\n\n[profile role]\nrole_arn = arn:aws:iam::123456789012:role/Dev\nsource_profile = default\n\n[profile process]\ncredential_process = /usr/local/bin/creds --profile x\n\n[profile region-only]\nregion = ap-northeast-1\n  s3 =\n    max_concurrent_requests = 20\n\n[sso-session corp]\nsso_start_url = https://corp.awsapps.com/start\n";
        let keys = |profile: &str| match profile_source(credentials, config, profile) {
            Some(Source::Static(keys)) => Some((
                keys.access_key,
                keys.session_token
                    .map(|token| token.expose_secret().to_owned()),
            )),
            _ => None,
        };
        assert_eq!(keys("default"), Some(("AKIDDEFAULT".into(), None)));
        assert_eq!(
            keys("work"),
            Some(("AKIDWORK".into(), Some("token".into())))
        );
        // Keys a config profile holds itself, which were not read before.
        assert_eq!(keys("keys-in-config"), Some(("AKIDCONFIG".into(), None)));
        // IAM Identity Center and assumed roles go through the AWS CLI.
        assert!(matches!(
            profile_source(credentials, config, "sso"),
            Some(Source::Cli)
        ));
        assert!(matches!(
            profile_source(credentials, config, "role"),
            Some(Source::Cli)
        ));
        assert!(matches!(
            profile_source(credentials, config, "process"),
            Some(Source::Process(command)) if command == "/usr/local/bin/creds --profile x"
        ));
        // A profile with a region and nothing to sign in with is none.
        assert!(profile_source(credentials, config, "region-only").is_none());
        assert!(profile_source(credentials, config, "missing").is_none());
        assert_eq!(
            section(config, "region-only", true)
                .get("region")
                .map(String::as_str),
            Some("ap-northeast-1")
        );
        assert_eq!(
            section(config, "default", true)
                .get("region")
                .map(String::as_str),
            Some("eu-west-1")
        );
        // `[sso-session corp]` is not a profile named `corp`.
        assert!(section(config, "corp", true).is_empty());
    }

    #[test]
    fn a_credential_process_answer_is_read_with_its_expiry() {
        let (credentials, expires) = parse_process_output(
            br#"{"Version":1,"AccessKeyId":"ASIAEXAMPLE","SecretAccessKey":"secret","SessionToken":"token","Expiration":"2026-11-11T12:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(credentials.access_key, "ASIAEXAMPLE");
        assert_eq!(credentials.secret_key.expose_secret(), "secret");
        assert_eq!(credentials.session_token.unwrap().expose_secret(), "token");
        assert_eq!(expires.unwrap().to_rfc3339(), "2026-11-11T12:00:00+00:00");
        assert!(parse_process_output(b"{\"Version\":1}").is_none());
        assert!(parse_process_output(b"not json").is_none());
    }

    #[tokio::test]
    async fn a_credential_process_runs_once_until_its_keys_near_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("count");
        let script = dir.path().join(if cfg!(windows) {
            "creds.cmd"
        } else {
            "creds.sh"
        });
        let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let answer = format!(
            r#"{{"Version":1,"AccessKeyId":"ASIAPROCESS","SecretAccessKey":"secret","Expiration":"{expires}"}}"#
        );
        if cfg!(windows) {
            std::fs::write(
                &script,
                format!(
                    "@echo off\r\necho x>> \"{}\"\r\necho {answer}\r\n",
                    count.display()
                ),
            )
            .unwrap();
        } else {
            std::fs::write(
                &script,
                format!("echo x >> '{}'\necho '{answer}'\n", count.display()),
            )
            .unwrap();
        }
        let command = if cfg!(windows) {
            format!("\"{}\"", script.display())
        } else {
            format!("sh '{}'", script.display())
        };
        let mut sign_in = AwsSignIn::new("default".into(), Source::Process(command));
        // A `default` profile nobody chose counts once it has worked.
        sign_in.chosen = false;
        assert!(!sign_in.usable());
        for _ in 0..3 {
            assert_eq!(
                sign_in.credentials().await.unwrap().access_key,
                "ASIAPROCESS"
            );
        }
        let runs = std::fs::read_to_string(&count).unwrap();
        assert_eq!(runs.lines().count(), 1, "{runs}");
        assert!(sign_in.usable());
        sign_in.forget();
        sign_in.credentials().await.unwrap();
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 2);
    }

    #[tokio::test]
    async fn a_failed_sign_in_says_what_to_run() {
        let sign_in = AwsSignIn::new(
            "corp".into(),
            Source::Process(if cfg!(windows) {
                "echo Token has expired and refresh failed 1>&2 & exit /b 1".into()
            } else {
                "echo 'Error when retrieving token from sso: Token has expired and refresh failed' >&2; exit 255".into()
            }),
        );
        let Err(error) = sign_in.credentials().await else {
            panic!("a failing credential_process gives no keys");
        };
        let error = error.to_string();
        assert!(
            error.contains("could not get AWS credentials for the AWS profile corp"),
            "{error}"
        );
        assert!(
            error.contains("Token has expired and refresh failed"),
            "{error}"
        );
        let cli = AwsSignIn::new("corp".into(), Source::Cli);
        assert!(
            cli.failure("Token has expired")
                .to_string()
                .contains("run `aws sso login --profile corp`")
        );
        // Without the AWS CLI, the words say to install it or use a key.
        if cfg!(unix) {
            let missing = AwsSignIn::new("corp".into(), Source::Cli);
            let Err(error) = missing.run("exit 127").await else {
                panic!("a missing AWS CLI gives no keys");
            };
            let error = error.to_string();
            assert!(error.contains("which isn't installed here"), "{error}");
            assert!(
                error.contains("connect Amazon Bedrock with an API key"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_short_term_api_key_names_its_region() {
        use base64::Engine as _;
        let url = "bedrock.amazonaws.com/?Action=CallWithBearerToken&X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLE%2F20261111%2Feu-west-1%2Fbedrock%2Faws4_request&X-Amz-Date=20261111T000000Z&X-Amz-Expires=43200&X-Amz-SignedHeaders=host&X-Amz-Signature=abc&Version=1";
        let key = format!(
            "bedrock-api-key-{}",
            base64::engine::general_purpose::STANDARD.encode(url)
        );
        assert_eq!(api_key_region(&key).as_deref(), Some("eu-west-1"));
        // A long-term key names no region; it works in every one.
        assert_eq!(api_key_region("ABSKQmVkcm9ja0FQSUtleS1leGFtcGxl"), None);
    }

    #[test]
    fn an_endpoint_names_its_region() {
        assert_eq!(
            endpoint_region("https://bedrock-runtime.eu-west-1.amazonaws.com").as_deref(),
            Some("eu-west-1")
        );
        assert_eq!(
            endpoint_region("https://bedrock-runtime-fips.us-gov-west-1.amazonaws.com/").as_deref(),
            Some("us-gov-west-1")
        );
        assert_eq!(endpoint_region("http://127.0.0.1:8080"), None);
        assert_eq!(endpoint_region("https://gateway.example.com/bedrock"), None);
    }
}
