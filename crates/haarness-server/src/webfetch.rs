//! What webfetch makes of a response: the bytes with their content coding
//! taken off and read in the character set they are written in, what kind
//! of file they hold, and, when a fetch fails, words that say what happened
//! and what to try next.

use std::io::Read as _;

use reqwest::header::HeaderMap;

/// Sent with every request of a markdown fetch: the page as Markdown where a
/// site offers it, then plain text, then HTML. Some sites answer a request
/// with no `Accept`, or `*/*` alone, with a 404 page or a bot check.
pub(crate) const ACCEPT_MARKDOWN: &str =
    "text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1";
/// For `format: raw`: the page as a browser is sent it.
pub(crate) const ACCEPT_RAW: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,text/plain;q=0.8,*/*;q=0.5";
pub(crate) const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";
/// The content codings [`decode_coding`] takes off.
pub(crate) const ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";

/// The largest zstd window a page may need: what HTTP's zstd coding allows
/// (RFC 9659), so a frame cannot make the decoder hold more.
const ZSTD_WINDOW: u64 = 8 * 1024 * 1024;

/// Why a body could not be decoded.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Undecodable {
    /// It decodes to more than the limit.
    TooLarge,
    /// A coding webfetch does not know.
    Unknown(String),
    /// The bytes are not in the coding the server named.
    Corrupt(String),
}

/// The codings a `Content-Encoding` lists, in the order they were applied.
fn codings(content_encoding: Option<&str>) -> Vec<String> {
    content_encoding
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(|coding| coding.trim().to_ascii_lowercase())
        .filter(|coding| !coding.is_empty() && coding != "identity")
        .collect()
}

/// `body` with the codings `content_encoding` names taken off, the last one
/// applied first: never more than `limit` bytes, however small `body` is.
pub(crate) fn decode_coding(
    content_encoding: Option<&str>,
    body: Vec<u8>,
    limit: usize,
) -> Result<Vec<u8>, Undecodable> {
    let mut body = body;
    for coding in codings(content_encoding).iter().rev() {
        body = decode_one(coding, &body, limit, false)?;
    }
    Ok(body)
}

/// As much of `head`, the start of a body, as decodes: for the words of an
/// error page, where what arrived is enough.
pub(crate) fn decode_head(content_encoding: Option<&str>, head: Vec<u8>, limit: usize) -> Vec<u8> {
    let mut body = head;
    for coding in codings(content_encoding).iter().rev() {
        match decode_one(coding, &body, limit, true) {
            Ok(decoded) => body = decoded,
            Err(_) => return Vec::new(),
        }
    }
    body
}

fn decode_one(
    coding: &str,
    body: &[u8],
    limit: usize,
    partial: bool,
) -> Result<Vec<u8>, Undecodable> {
    let mut out = Vec::new();
    let read = match coding {
        "gzip" | "x-gzip" => read_capped(flate2::read::MultiGzDecoder::new(body), limit, &mut out),
        // Servers send deflate both as RFC 1950 says (zlib) and bare.
        "deflate" if zlib_header(body) => {
            read_capped(flate2::read::ZlibDecoder::new(body), limit, &mut out)
        }
        "deflate" => read_capped(flate2::read::DeflateDecoder::new(body), limit, &mut out),
        "br" => read_capped(
            brotli_decompressor::Decompressor::new(body, 8192),
            limit,
            &mut out,
        ),
        "zstd" => ruzstd::decoding::StreamingDecoder::new_with_max_window_size(body, ZSTD_WINDOW)
            .map_err(std::io::Error::other)
            .and_then(|decoder| read_capped(decoder, limit, &mut out)),
        other => return Err(Undecodable::Unknown(other.to_owned())),
    };
    match read {
        Ok(true) => Ok(out),
        // The start of an error page is all that is wanted of it.
        _ if partial => Ok(out),
        Ok(false) => Err(Undecodable::TooLarge),
        Err(_) => Err(Undecodable::Corrupt(coding.to_owned())),
    }
}

/// Reads `reader` into `out`, at most `limit` bytes: whether it ended
/// within them.
fn read_capped(
    reader: impl std::io::Read,
    limit: usize,
    out: &mut Vec<u8>,
) -> std::io::Result<bool> {
    reader.take(limit as u64 + 1).read_to_end(out)?;
    if out.len() > limit {
        out.truncate(limit);
        return Ok(false);
    }
    Ok(true)
}

fn zlib_header(body: &[u8]) -> bool {
    matches!(body, [first, second, ..]
        if first & 0x0f == 8 && (u16::from(*first) << 8 | u16::from(*second)) % 31 == 0)
}

/// What a fetched body holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Text; `html` when it is a web page to turn into Markdown, `xml` when
    /// it may name its character set in an XML declaration.
    Text {
        html: bool,
        xml: bool,
    },
    Pdf,
    /// An image a model can be shown, with its file extension.
    Image(&'static str),
    /// Any other file.
    Other,
}

/// The type `content_type` names, in lower case without its parameters.
pub(crate) fn mime(content_type: Option<&str>) -> String {
    content_type
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

/// What `body` holds, served as `content_type`. A type that says nothing
/// (none, or `application/octet-stream`, which servers give raw source
/// files too) is told from the bytes.
pub(crate) fn kind(content_type: Option<&str>, body: &[u8]) -> Kind {
    let mime = mime(content_type);
    let sniffed = sniff(body);
    if sniffed == Some(Kind::Pdf) {
        return Kind::Pdf;
    }
    let text = |mime: &str| Kind::Text {
        html: matches!(mime, "text/html" | "application/xhtml+xml") || starts_like_html(body),
        xml: mime.ends_with("xml") || body.starts_with(b"<?xml"),
    };
    match mime.as_str() {
        ""
        | "application/octet-stream"
        | "binary/octet-stream"
        | "application/unknown"
        | "application/x-download"
        | "application/force-download"
        | "unknown/unknown" => sniffed.unwrap_or_else(|| {
            if looks_like_text(body) {
                text("")
            } else {
                Kind::Other
            }
        }),
        "application/pdf" | "application/x-pdf" => Kind::Pdf,
        "image/png" => Kind::Image("png"),
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Kind::Image("jpg"),
        "image/gif" => Kind::Image("gif"),
        "image/webp" => Kind::Image("webp"),
        textual if is_textual(textual) => text(textual),
        _ => sniffed.unwrap_or(Kind::Other),
    }
}

fn is_textual(mime: &str) -> bool {
    mime.starts_with("text/")
        || [
            "+json",
            "+xml",
            "+yaml",
            "/json",
            "/xml",
            "/javascript",
            "/ecmascript",
            "/x-sh",
            "/x-shellscript",
            "/yaml",
            "/x-yaml",
            "/toml",
            "/x-toml",
            "/graphql",
            "/sql",
            "/x-ndjson",
            "/jsonl",
            "/x-www-form-urlencoded",
            "/x-tex",
            "/x-latex",
            "/typescript",
            "/x-typescript",
            "/x-python",
            "/x-httpd-php",
            "/x-perl",
            "/x-ruby",
            "/rtf",
        ]
        .iter()
        .any(|ending| mime.ends_with(ending))
}

/// A PDF or an image known by its first bytes.
fn sniff(body: &[u8]) -> Option<Kind> {
    if body.starts_with(b"%PDF-") {
        Some(Kind::Pdf)
    } else if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Kind::Image("png"))
    } else if body.starts_with(b"\xff\xd8\xff") {
        Some(Kind::Image("jpg"))
    } else if body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a") {
        Some(Kind::Image("gif"))
    } else if body.len() > 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP" {
        Some(Kind::Image("webp"))
    } else {
        None
    }
}

/// Whether a body starts as an HTML document does, whatever its type says.
pub(crate) fn starts_like_html(body: &[u8]) -> bool {
    let start = body
        .iter()
        .position(|byte| {
            !byte.is_ascii_whitespace() && *byte != 0xef && *byte != 0xbb && *byte != 0xbf
        })
        .unwrap_or(body.len());
    let head = &body[start..body.len().min(start + 15)];
    let head = head.to_ascii_lowercase();
    head.starts_with(b"<!doctype html") || head.starts_with(b"<html")
}

/// Whether the start of `body` reads as text: no NUL byte, and UTF-8 (a
/// character cut at the end aside) or almost all printable ASCII.
fn looks_like_text(body: &[u8]) -> bool {
    let head = &body[..body.len().min(8192)];
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(error) if error.error_len().is_none() => true,
        Err(_) => {
            let printable = head
                .iter()
                .filter(|byte| byte.is_ascii_graphic() || byte.is_ascii_whitespace())
                .count();
            printable * 20 >= head.len() * 19
        }
    }
}

/// `body` as text, in the character set a byte order mark names, else the
/// one `content_type` names, else the one an HTML page's `<meta>` or an XML
/// declaration names, else UTF-8, or windows-1252 for bytes that are mostly
/// not UTF-8, as browsers read a page that names none.
pub(crate) fn decode_text(body: &[u8], content_type: Option<&str>, kind: Kind) -> String {
    let (html, xml) = match kind {
        Kind::Text { html, xml } => (html, xml),
        _ => (false, false),
    };
    let from_header = content_type
        .and_then(charset_parameter)
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()));
    let from_document = || {
        let label = if html { meta_charset(body) } else { None }.or_else(|| {
            if xml || html {
                xml_encoding(body)
            } else {
                None
            }
        })?;
        let encoding = encoding_rs::Encoding::for_label(label.as_bytes())?;
        // A document read as ASCII to find its declaration is not UTF-16.
        Some(
            if encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE {
                encoding_rs::UTF_8
            } else {
                encoding
            },
        )
    };
    let encoding = from_header
        .or_else(from_document)
        .filter(|encoding| *encoding != encoding_rs::REPLACEMENT)
        .unwrap_or_else(|| undeclared_encoding(body));
    // Sniffs a byte order mark, which wins, and leaves it out.
    encoding.decode(body).0.into_owned()
}

fn undeclared_encoding(body: &[u8]) -> &'static encoding_rs::Encoding {
    let (text, malformed) = encoding_rs::UTF_8.decode_without_bom_handling(body);
    if !malformed {
        return encoding_rs::UTF_8;
    }
    let (mut wide, mut broken) = (0_usize, 0_usize);
    for character in text.chars() {
        if character == char::REPLACEMENT_CHARACTER {
            broken += 1;
        } else if !character.is_ascii() {
            wide += 1;
        }
    }
    if wide > broken {
        encoding_rs::UTF_8
    } else {
        encoding_rs::WINDOWS_1252
    }
}

/// The `charset` parameter of a `Content-Type`.
fn charset_parameter(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['"', '\'']).to_owned())
            .filter(|value| !value.is_empty())
    })
}

/// The character set a `<meta charset>` or `<meta http-equiv>` in the head
/// of a page names.
fn meta_charset(body: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&body[..body.len().min(16 * 1024)]).to_ascii_lowercase();
    let mut rest = head.as_str();
    while let Some(at) = rest.find("<meta") {
        rest = &rest[at + 5..];
        let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
        if let Some(label) = value_after(tag, "charset") {
            return Some(label);
        }
    }
    None
}

/// The encoding an XML declaration at the start of `body` names.
fn xml_encoding(body: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&body[..body.len().min(512)]);
    let declaration = head.trim_start_matches('\u{feff}').strip_prefix("<?xml")?;
    let declaration = &declaration[..declaration.find("?>")?];
    value_after(&declaration.to_ascii_lowercase(), "encoding")
}

/// The value of `name=` in `text`, quoted or not.
fn value_after(text: &str, name: &str) -> Option<String> {
    let at = text.find(name)?;
    let rest = text[at + name.len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.trim_start_matches(['"', '\'']);
    let label: String = rest
        .chars()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
        .collect();
    (!label.is_empty()).then_some(label)
}

/// The service whose bot check or firewall answered instead of the page,
/// when one did: by the header its challenges carry, or by the marks of its
/// challenge page, which is small (a page about such checks is not one).
pub(crate) fn bot_check(status: u16, headers: &HeaderMap, body: &[u8]) -> Option<&'static str> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_ascii_lowercase)
    };
    if header("cf-mitigated").as_deref() == Some("challenge") {
        return Some("Cloudflare");
    }
    if header("x-vercel-mitigated").as_deref() == Some("challenge") {
        return Some("Vercel");
    }
    let small = if status < 300 { 64 * 1024 } else { 512 * 1024 };
    if body.len() > small {
        return None;
    }
    let page = String::from_utf8_lossy(body).to_ascii_lowercase();
    let marks: [(&str, &[&str]); 7] = [
        (
            "Cloudflare",
            &["window._cf_chl_opt", "<title>just a moment...</title>"],
        ),
        ("Fastly", &["/_fs-ch-"]),
        ("Vercel", &["vercel security checkpoint"]),
        ("AWS WAF", &["awswafintegration", "awswafcookiedomainlist"]),
        ("DataDome", &["captcha-delivery.com/captcha"]),
        ("HUMAN", &["px-captcha"]),
        ("Akamai", &["errors.edgesuite.net"]),
    ];
    marks
        .iter()
        .find(|(_, marks)| marks.iter().any(|mark| page.contains(mark)))
        .map(|(service, _)| *service)
}

/// Where the same content can usually be read when `url`'s page cannot (a
/// bot check, a page built by JavaScript, a rate limit): the site's API or
/// raw source, for the sites agents read most. None for any other page.
pub(crate) fn other_source(url: &url::Url) -> Option<String> {
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let parts: Vec<&str> = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect();
    let site = match host {
        "stackoverflow.com" => Some("stackoverflow".to_owned()),
        "superuser.com" | "serverfault.com" | "askubuntu.com" | "mathoverflow.net"
        | "stackapps.com" => Some(host.split('.').next()?.to_owned()),
        _ => host
            .strip_suffix(".stackexchange.com")
            .or_else(|| {
                host.strip_suffix(".stackoverflow.com")
                    .map(|language| language.trim_end_matches('.'))
            })
            .map(|name| {
                if host.ends_with(".stackoverflow.com") {
                    format!("{name}.stackoverflow")
                } else {
                    name.to_owned()
                }
            }),
    };
    if let Some(site) = site {
        let id = |at: usize| {
            parts
                .get(at)
                .filter(|id| id.bytes().all(|byte| byte.is_ascii_digit()))
                .copied()
        };
        return match parts.first().copied() {
            Some("questions" | "q") => id(1).map(|id| {
                let api = "https://api.stackexchange.com/2.3/questions";
                format!(
                    "Fetch {api}/{id}?site={site}&filter=withbody instead: the Stack Exchange \
                     API has the same question, and its answers are at \
                     {api}/{id}/answers?site={site}&filter=withbody&sort=votes."
                )
            }),
            Some("a") => id(1).map(|id| {
                format!(
                    "Fetch https://api.stackexchange.com/2.3/answers/{id}?site={site}&filter=withbody \
                     instead: the Stack Exchange API has the same answer."
                )
            }),
            _ => None,
        };
    }
    let number = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    match (host, parts.as_slice()) {
        ("npmjs.com", ["package", scope, name, ..]) if scope.starts_with('@') => Some(format!(
            "Fetch https://registry.npmjs.org/{scope}/{name}/latest instead: the npm registry \
             has the package's details as JSON."
        )),
        ("npmjs.com", ["package", name, ..]) => Some(format!(
            "Fetch https://registry.npmjs.org/{name}/latest instead: the npm registry has the \
             package's details as JSON."
        )),
        ("pypi.org", ["project", name, version, ..]) => Some(format!(
            "Fetch https://pypi.org/pypi/{name}/{version}/json instead: PyPI's JSON API has the \
             project's details."
        )),
        ("pypi.org", ["project", name]) => Some(format!(
            "Fetch https://pypi.org/pypi/{name}/json instead: PyPI's JSON API has the project's \
             details."
        )),
        ("crates.io", ["crates", name, ..]) => Some(format!(
            "Fetch https://crates.io/api/v1/crates/{name} instead: the crates.io API has the \
             crate's details."
        )),
        ("github.com", [owner, repository, "blob", rest @ ..]) if !rest.is_empty() => {
            Some(format!(
                "Fetch https://raw.githubusercontent.com/{owner}/{repository}/{} instead: it is \
                 the same file as plain text.",
                rest.join("/")
            ))
        }
        ("github.com", [owner, repository, "issues", issue]) if number(issue) => Some(format!(
            "Fetch https://api.github.com/repos/{owner}/{repository}/issues/{issue} instead: the \
             GitHub API has the same issue, and its comments are at \
             https://api.github.com/repos/{owner}/{repository}/issues/{issue}/comments."
        )),
        ("github.com", [owner, repository, "pull", pull, ..]) if number(pull) => Some(format!(
            "Fetch https://api.github.com/repos/{owner}/{repository}/pulls/{pull} instead: the \
             GitHub API has the same pull request."
        )),
        _ => None,
    }
}

/// What to try when `url`'s page cannot be read as it is served.
fn try_instead(url: &url::Url) -> String {
    other_source(url).unwrap_or_else(|| {
        "Look for the same content in the site's API or raw source if it has one, or on another \
         site."
            .to_owned()
    })
}

/// A note for a page that came with almost no text: most likely one a
/// browser builds with JavaScript, which webfetch does not run.
pub(crate) fn thin_page_note(url: &url::Url, html_bytes: usize, markdown: &str) -> Option<String> {
    let words = markdown.split_whitespace().count();
    if html_bytes < 2048 || words >= 40 {
        return None;
    }
    Some(format!(
        "[This page has almost no text without JavaScript ({words} words from {} of HTML): it \
         most likely builds its content in the browser, and webfetch does not run scripts. {} \
         Browser tools, if this chat has them, can open it.]",
        size(html_bytes as u64),
        try_instead(url)
    ))
}

/// A size in words: `812 bytes`, `14 KB`, `2.1 MB`.
pub(crate) fn size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes < KB {
        format!("{bytes} bytes")
    } else if bytes < MB {
        format!("{} KB", bytes.div_ceil(KB))
    } else {
        format!("{}.{} MB", bytes / MB, (bytes % MB) * 10 / MB)
    }
}

/// Why a fetch of `url` that the server answered with `status` failed, and
/// what to try: the words the model gets. `said` is the start of the
/// error page's text.
pub(crate) fn status_failure(
    url: &url::Url,
    status: reqwest::StatusCode,
    headers: &HeaderMap,
    said: &str,
) -> String {
    let code = status.as_u16();
    let lead = match status.canonical_reason() {
        Some(reason) => format!("{url} answered HTTP {code} ({reason})"),
        None => format!("{url} answered HTTP {code}"),
    };
    let why = match code {
        400 => "the server did not accept the request as written. Check the address and its \
                query parameters."
            .to_owned(),
        401 => "the page needs a sign-in, and webfetch sends none. Read a public page or API \
                instead, or ask the user to paste the content."
            .to_owned(),
        403 => format!(
            "the server refused to show this page: it may need a sign-in, or turn away \
             automated readers. {}",
            try_instead(url)
        ),
        404 | 410 => "there is no page at this address. Check it for typos; if the page has \
                      moved, look for its current address (a search, or the site's own \
                      links). The same address will not work if fetched again."
            .to_owned(),
        405 | 406 | 415 => format!(
            "the server would not send this page to a plain read. {}",
            try_instead(url)
        ),
        407 => "the proxy this computer reaches the web through asks for a sign-in. Put its \
                user name and password in the proxy address in HTTPS_PROXY (and HTTP_PROXY), \
                or ask the user to."
            .to_owned(),
        429 => {
            let wait = headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .map(|value| match value.parse::<u64>() {
                    Ok(seconds) => format!(" It asks to wait {seconds} seconds."),
                    Err(_) => format!(" It asks to wait until {value}."),
                })
                .unwrap_or_default();
            format!(
                "the site limits how often it can be read, and this computer has reached that \
                 limit for now.{wait} {}",
                try_instead(url)
            )
        }
        451 => "the site withholds this page for legal reasons where this computer is.".to_owned(),
        408 | 502..=504 | 520..=530 => "the server, or a gateway in front of it, did not answer \
                                       in time or is down. This is on the site's side: try \
                                       again in a little while, or read the content from \
                                       another source."
            .to_owned(),
        500..=599 => "the server failed while answering. This is on the site's side: try again \
                      in a little while, or read the content from another source."
            .to_owned(),
        _ => "the server did not send the page.".to_owned(),
    };
    let said = excerpt(said, 600);
    if said.is_empty() {
        format!("{lead}: {why}")
    } else {
        format!("{lead}: {why}\nThe server said: {said}")
    }
}

/// `text` on one line, at most `limit` characters.
fn excerpt(text: &str, limit: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(limit) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    }
}

/// Why a fetch of `url` that a bot check from `service` answered failed.
pub(crate) fn bot_check_failure(url: &url::Url, status: u16, service: &str) -> String {
    format!(
        "{url} answered with a bot check from {service} (HTTP {status}) instead of the page. \
         It lets only a web browser through, so webfetch cannot read this page, and fetching \
         it again will not help. {} Browser tools, if this chat has them, can open the page.",
        try_instead(url)
    )
}

/// Why `url` could not be read: larger than `limit` bytes (`declared` when
/// the server said how large).
pub(crate) fn too_large_failure(url: &url::Url, limit: usize, declared: Option<u64>) -> String {
    let declared = declared
        .filter(|length| *length > limit as u64)
        .map(|length| format!(" (it is {})", size(length)))
        .unwrap_or_default();
    format!(
        "{url} is larger than the {} webfetch reads{declared}. Fetch a smaller page (large \
         documents often have a page per section), or download it with a shell command such \
         as curl -L -o page.html '{url}' and search the file.",
        size(limit as u64)
    )
}

/// Why `url` could not be read: its coding could not be taken off.
pub(crate) fn undecodable_failure(url: &url::Url, why: &Undecodable, limit: usize) -> String {
    match why {
        Undecodable::TooLarge => too_large_failure(url, limit, None),
        Undecodable::Unknown(coding) => format!(
            "{url} came compressed as \"{coding}\", which webfetch cannot unpack. Download it \
             with a shell command such as curl --compressed -L -o page '{url}' if you need it."
        ),
        Undecodable::Corrupt(coding) => format!(
            "{url} said its page was compressed with {coding}, but the bytes that came are not, \
             so it could not be read. Try again; if it fails the same way, the server is \
             sending a broken page."
        ),
    }
}

/// Why `url` did not finish loading within `seconds`.
pub(crate) fn timeout_failure(url: &url::Url, seconds: u64) -> String {
    format!(
        "{url} did not finish loading within {seconds} seconds. The server is slow or not \
         answering: try again in a little while, or read the content from another source."
    )
}

/// Why a request to `url` got no answer: what the network, name lookup or
/// TLS said, in plain words, from the causes `error` carries.
pub(crate) async fn send_failure(url: &url::Url, error: &reqwest::Error) -> String {
    use std::io::ErrorKind;
    let host = url.host_str().unwrap_or_default().to_owned();
    let port = url.port_or_known_default().unwrap_or(443);
    let local = url.host().is_some_and(|host| match host {
        url::Host::Domain(name) => {
            name.eq_ignore_ascii_case("localhost")
                || name.to_ascii_lowercase().ends_with(".localhost")
        }
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    });
    let mut causes = Vec::new();
    let mut tls = None;
    let mut io = None;
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(cause) = source {
        causes.push(cause.to_string());
        if let Some(found) = cause.downcast_ref::<rustls::Error>() {
            tls.get_or_insert_with(|| found.clone());
        }
        if let Some(found) = cause.downcast_ref::<std::io::Error>() {
            io.get_or_insert((found.kind(), found.to_string()));
            // An I/O error holds the TLS error it reports, sometimes inside
            // another I/O error, without naming it as its source.
            let mut inner = found.get_ref();
            while let Some(held) = inner {
                if let Some(found) = held.downcast_ref::<rustls::Error>() {
                    tls.get_or_insert_with(|| found.clone());
                    break;
                }
                inner = held
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::get_ref);
            }
        }
        source = cause.source();
    }
    let all = causes.join(": ").to_ascii_lowercase();
    let innermost = causes.last().cloned().unwrap_or_default();
    if let Some(tls) = tls {
        return tls_failure(&host, &tls);
    }
    if all.contains("dns error")
        || all.contains("failed to lookup address")
        || all.contains("no such host is known")
        || all.contains("name or service not known")
        || all.contains("nodename nor servname")
    {
        return format!(
            "Could not find the site {host}: the name does not resolve (the DNS lookup \
             failed). Check the address for typos. If it is right, this computer may be \
             offline, its DNS server may not know the name, or the site may have only IPv6 \
             addresses on a network without IPv6."
        );
    }
    if error.is_timeout() {
        return format!(
            "Connecting to {host} took too long, so the fetch gave up. The site may be down or \
             unreachable from this network: try again later, or read the content from another \
             source."
        );
    }
    match io.as_ref().map(|(kind, _)| *kind) {
        Some(ErrorKind::ConnectionRefused) if local => format!(
            "Nothing is listening at {host}:{port} on this computer (the connection was \
             refused). Start the server that should answer there, or check its port."
        ),
        Some(ErrorKind::ConnectionRefused) => format!(
            "{host}:{port} refused the connection: no server is listening there, or a firewall \
             turns this computer away. Check the address and port, or try again later."
        ),
        Some(ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable) => {
            let only_ipv6 = match url.host() {
                Some(url::Host::Ipv6(_)) => true,
                Some(url::Host::Domain(name)) => tokio::net::lookup_host((name, port))
                    .await
                    .is_ok_and(|mut addresses| {
                        let mut any = false;
                        let all_v6 = addresses.all(|address| {
                            any = true;
                            address.is_ipv6()
                        });
                        any && all_v6
                    }),
                _ => false,
            };
            let why = if only_ipv6 {
                " The site has IPv6 addresses only, and this computer's network seems to have \
                 no IPv6 connection: read the content from another source."
            } else {
                " Check this computer's network connection, VPN or proxy."
            };
            format!("This computer has no route to {host} ({innermost}).{why}")
        }
        Some(ErrorKind::TimedOut) => format!(
            "Connecting to {host} timed out. The site may be down or unreachable from this \
             network: try again later, or read the content from another source."
        ),
        Some(
            ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::UnexpectedEof
            | ErrorKind::BrokenPipe,
        ) => format!(
            "The connection to {host} closed before the page arrived ({innermost}). Try \
             again; if it keeps happening, a firewall, proxy or the site itself is cutting \
             the connection."
        ),
        Some(ErrorKind::PermissionDenied) => format!(
            "This computer did not allow the connection to {host} ({innermost}): a firewall or \
             the system's network privacy settings block it."
        ),
        _ if all.contains("proxy") => format!(
            "The proxy this computer reaches the web through did not pass the request on to \
             {host} ({innermost}). Check the proxy set in HTTPS_PROXY or HTTP_PROXY."
        ),
        _ => format!(
            "Could not reach {host} ({innermost}). Try again, or read the content from another \
             source."
        ),
    }
}

fn tls_failure(host: &str, error: &rustls::Error) -> String {
    use rustls::CertificateError;
    let why = match error {
        rustls::Error::InvalidCertificate(certificate) => match certificate {
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
                "its certificate has expired".to_owned()
            }
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
                "its certificate is not valid yet (if the site is fine, this computer's clock \
                 may be wrong)"
                    .to_owned()
            }
            CertificateError::UnknownIssuer => "its certificate comes from an authority this \
                                                computer does not trust. On a network that \
                                                inspects secure traffic (a company proxy or \
                                                antivirus), that authority's root certificate \
                                                must be in the system's trusted certificates"
                .to_owned(),
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
                format!("its certificate is for another site, not {host}")
            }
            CertificateError::Revoked => "its certificate has been revoked".to_owned(),
            // The system's verifier gives its own words for what it found.
            CertificateError::Other(found) => {
                let said = found.to_string();
                let lower = said.to_ascii_lowercase();
                if lower.contains("expired") {
                    "its certificate has expired".to_owned()
                } else if lower.contains("revoked") {
                    "its certificate has been revoked".to_owned()
                } else if [
                    "not trusted",
                    "untrusted",
                    "self signed",
                    "self-signed",
                    "unknown issuer",
                    "root",
                ]
                .iter()
                .any(|word| lower.contains(word))
                {
                    format!(
                        "its certificate comes from an authority this computer does not trust \
                         ({said}). On a network that inspects secure traffic (a company proxy or \
                         antivirus), that authority's root certificate must be in the system's \
                         trusted certificates"
                    )
                } else if lower.contains("host name")
                    || lower.contains("hostname")
                    || lower.contains("name mismatch")
                {
                    format!("its certificate is for another site, not {host}")
                } else {
                    format!("its certificate could not be verified ({said})")
                }
            }
            other => format!("its certificate could not be verified ({other:?})"),
        },
        other => format!("the two sides could not agree on a secure connection ({other})"),
    };
    format!(
        "The secure connection to {host} failed: {why}. webfetch never reads a page whose \
         connection is not secure; read the content from another source, or tell the user \
         about the site's certificate."
    )
}

/// Why the body of `url` stopped arriving.
pub(crate) fn body_failure(url: &url::Url, error: &reqwest::Error) -> String {
    let mut innermost = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        innermost = cause.to_string();
        source = cause.source();
    }
    format!(
        "The connection to {} broke while the page was arriving ({innermost}). Try again; if \
         it keeps happening, the server or a proxy is cutting it off.",
        url.host_str().unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn codings_come_off_in_the_order_they_were_put_on() {
        let page = b"<html><body><p>compressed page</p></body></html>".repeat(20);
        assert_eq!(
            decode_coding(Some("gzip"), gzip(&page), 1 << 20).unwrap(),
            page
        );
        assert_eq!(
            decode_coding(Some("x-gzip"), gzip(&page), 1 << 20).unwrap(),
            page
        );
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zlib.write_all(&page).unwrap();
        assert_eq!(
            decode_coding(Some("deflate"), zlib.finish().unwrap(), 1 << 20).unwrap(),
            page
        );
        let mut bare = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        bare.write_all(&page).unwrap();
        assert_eq!(
            decode_coding(Some("Deflate"), bare.finish().unwrap(), 1 << 20).unwrap(),
            page
        );
        // Twice over: gzip put on first, so taken off last.
        assert_eq!(
            decode_coding(Some("gzip, gzip"), gzip(&gzip(&page)), 1 << 20).unwrap(),
            page
        );
        assert_eq!(decode_coding(None, page.clone(), 1 << 20).unwrap(), page);
        assert_eq!(
            decode_coding(Some("identity"), page.clone(), 1 << 20).unwrap(),
            page
        );
        assert_eq!(
            decode_coding(Some("compress"), page.clone(), 1 << 20),
            Err(Undecodable::Unknown("compress".into()))
        );
        assert_eq!(
            decode_coding(Some("gzip"), page.clone(), 1 << 20),
            Err(Undecodable::Corrupt("gzip".into()))
        );
    }

    #[test]
    fn brotli_and_zstd_come_off() {
        // Eight lines of "brotli and zstd fixture text", made with
        // `brotli -q 5` and `zstd -3`.
        let text = b"brotli and zstd fixture text\n".repeat(8);
        let brotli = [
            0xa1, 0x38, 0x07, 0x00, 0x20, 0x10, 0xbb, 0xdc, 0x5b, 0xa6, 0xe6, 0x39, 0x15, 0x11,
            0x4f, 0x7f, 0x4f, 0xbe, 0xe0, 0x99, 0xb0, 0xac, 0x95, 0x36, 0x87, 0xae, 0x1f, 0x61,
            0x14, 0xfe, 0xc5, 0x35, 0x6b, 0x79, 0x13, 0x4a, 0x2c, 0x02, 0x00,
        ];
        assert_eq!(
            decode_coding(Some("br"), brotli.to_vec(), 1024).unwrap(),
            text
        );
        let zstd = [
            0x28, 0xb5, 0x2f, 0xfd, 0x24, 0xe8, 0x25, 0x01, 0x00, 0xe8, 0x62, 0x72, 0x6f, 0x74,
            0x6c, 0x69, 0x20, 0x61, 0x6e, 0x64, 0x20, 0x7a, 0x73, 0x74, 0x64, 0x20, 0x66, 0x69,
            0x78, 0x74, 0x75, 0x72, 0x65, 0x20, 0x74, 0x65, 0x78, 0x74, 0x0a, 0x01, 0x00, 0x21,
            0x01, 0xa5, 0x9c, 0xdb, 0x40, 0x48, 0xce,
        ];
        assert_eq!(
            decode_coding(Some("zstd"), zstd.to_vec(), 1024).unwrap(),
            text
        );
        assert_eq!(
            decode_coding(Some("br"), brotli.to_vec(), 100),
            Err(Undecodable::TooLarge)
        );
    }

    #[test]
    fn a_small_body_that_unpacks_past_the_limit_is_too_large() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(&vec![b'a'; 8 * 1024 * 1024]).unwrap();
        let bomb = encoder.finish().unwrap();
        assert!(bomb.len() < 64 * 1024, "{}", bomb.len());
        assert_eq!(
            decode_coding(Some("gzip"), bomb.clone(), 1024 * 1024),
            Err(Undecodable::TooLarge)
        );
        // The start of an error page is enough.
        assert_eq!(decode_head(Some("gzip"), bomb, 4096).len(), 4096);
        let page = gzip(b"rate limit exceeded");
        assert_eq!(
            decode_head(Some("gzip"), page[..page.len() - 4].to_vec(), 4096),
            b"rate limit exceeded"
        );
    }

    #[test]
    fn text_is_read_in_the_character_set_it_names() {
        let html = Kind::Text {
            html: true,
            xml: false,
        };
        let plain = Kind::Text {
            html: false,
            xml: false,
        };
        // こころ in Shift_JIS, named only by the page's meta.
        let sjis = b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html;charset=Shift_JIS\" /></head><body>\x82\xb1\x82\xb1\x82\xeb</body></html>";
        assert!(decode_text(sjis, Some("text/html"), html).contains("こころ"));
        // Привет in KOI8-R, named by the header.
        let koi8 = b"\xf0\xd2\xc9\xd7\xc5\xd4";
        assert_eq!(
            decode_text(koi8, Some("text/plain; charset=koi8-r"), plain),
            "Привет"
        );
        assert_eq!(
            decode_text(koi8, Some("text/plain; charset=\"KOI8-R\""), plain),
            "Привет"
        );
        // The header wins over the page, and a byte order mark over both.
        let meta_says_sjis = b"<meta charset=shift_jis>caf\xc3\xa9";
        assert!(
            decode_text(meta_says_sjis, Some("text/html; charset=utf-8"), html).ends_with("café")
        );
        assert_eq!(
            decode_text(
                b"\xef\xbb\xbfcaf\xc3\xa9",
                Some("text/plain; charset=latin1"),
                plain
            ),
            "café"
        );
        // An XML declaration names it for XML.
        let xml = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a>caf\xe9</a>";
        assert!(
            decode_text(
                xml,
                Some("application/xml"),
                Kind::Text {
                    html: false,
                    xml: true
                }
            )
            .contains("café")
        );
        // Named nowhere: UTF-8, or windows-1252 for Latin-1 bytes.
        assert_eq!(decode_text("naïve ☃".as_bytes(), None, plain), "naïve ☃");
        assert_eq!(decode_text(b"caf\xe9 cr\xe8me", None, plain), "café crème");
        // Mostly UTF-8 with one stray byte stays UTF-8.
        assert_eq!(
            decode_text(b"\xe6\x97\xa5\xe6\x9c\xac \xff", None, plain),
            "日本 \u{fffd}"
        );
        // A byte order mark is not text.
        assert_eq!(
            decode_text(b"\xef\xbb\xbfRFC", Some("text/plain;charset=utf-8"), plain),
            "RFC"
        );
        // A label for UTF-16 inside a page read as ASCII means UTF-8.
        assert_eq!(
            decode_text(b"<meta charset=utf-16>ok", None, html),
            "<meta charset=utf-16>ok"
        );
    }

    #[test]
    fn the_kind_of_a_body_comes_from_its_type_or_its_bytes() {
        let text = Kind::Text {
            html: false,
            xml: false,
        };
        let page = Kind::Text {
            html: true,
            xml: false,
        };
        assert_eq!(kind(Some("text/html; charset=utf-8"), b"<p>x</p>"), page);
        assert_eq!(kind(Some("text/plain"), b"<!DOCTYPE html><p>x</p>"), page);
        assert_eq!(kind(Some("application/json"), b"{}"), text);
        assert_eq!(kind(Some("application/problem+json"), b"{}"), text);
        assert_eq!(
            kind(Some("image/svg+xml"), b"<svg/>"),
            Kind::Text {
                html: false,
                xml: true
            }
        );
        assert_eq!(kind(Some("application/pdf"), b"%PDF-1.7"), Kind::Pdf);
        assert_eq!(kind(Some("text/plain"), b"%PDF-1.7 ..."), Kind::Pdf);
        assert_eq!(
            kind(Some("image/png"), b"\x89PNG\r\n\x1a\n...."),
            Kind::Image("png")
        );
        assert_eq!(
            kind(Some("image/jpeg"), b"\xff\xd8\xff..."),
            Kind::Image("jpg")
        );
        // Servers send source files as octet-stream, and some send no type.
        assert_eq!(
            kind(Some("application/octet-stream"), b"fn main() {}\n"),
            text
        );
        assert_eq!(kind(None, b"plain words"), text);
        assert_eq!(kind(None, b"GIF89a...."), Kind::Image("gif"));
        assert_eq!(
            kind(
                Some("application/octet-stream"),
                b"\x7fELF\x02\x01\x01\0\0\0"
            ),
            Kind::Other
        );
        assert_eq!(kind(Some("application/zip"), b"PK\x03\x04"), Kind::Other);
        assert_eq!(kind(Some("image/avif"), b"\0\0\0 ftypavif"), Kind::Other);
    }

    #[test]
    fn bot_checks_are_told_from_pages() {
        let mut headers = HeaderMap::new();
        assert_eq!(bot_check(403, &headers, b"<p>Forbidden</p>"), None);
        headers.insert("cf-mitigated", "challenge".parse().unwrap());
        assert_eq!(bot_check(403, &headers, b""), Some("Cloudflare"));
        let headers = HeaderMap::new();
        let fastly = b"<html><head><link href=\"/_fs-ch-1T1wmsGaOgGaSxcX/assets/styles.css\"><title>Client Challenge</title>";
        assert_eq!(bot_check(200, &headers, fastly), Some("Fastly"));
        let cloudflare = b"<title>Just a moment...</title><script>window._cf_chl_opt={}</script>";
        assert_eq!(bot_check(503, &headers, cloudflare), Some("Cloudflare"));
        // A long article that quotes such a page is the page asked for.
        let mut article = b"<article>".repeat(10_000);
        article.extend_from_slice(cloudflare);
        assert_eq!(bot_check(200, &headers, &article), None);
    }

    #[test]
    fn other_sources_are_offered_for_the_sites_agents_read_most() {
        let source = |url: &str| other_source(&url::Url::parse(url).unwrap()).unwrap_or_default();
        assert_eq!(
            source("https://stackoverflow.com/questions/11227809/why-is-it"),
            "Fetch https://api.stackexchange.com/2.3/questions/11227809?site=stackoverflow&filter=withbody \
             instead: the Stack Exchange API has the same question, and its answers are at \
             https://api.stackexchange.com/2.3/questions/11227809/answers?site=stackoverflow&filter=withbody&sort=votes."
        );
        for (page, fetch) in [
            (
                "https://unix.stackexchange.com/q/42/",
                "questions/42?site=unix&",
            ),
            (
                "https://ru.stackoverflow.com/a/7",
                "answers/7?site=ru.stackoverflow&",
            ),
            (
                "https://superuser.com/questions/1/x",
                "questions/1?site=superuser&",
            ),
            (
                "https://www.npmjs.com/package/react",
                "Fetch https://registry.npmjs.org/react/latest instead",
            ),
            (
                "https://www.npmjs.com/package/@types/node/v/1.0.0",
                "Fetch https://registry.npmjs.org/@types/node/latest instead",
            ),
            (
                "https://pypi.org/project/requests/",
                "Fetch https://pypi.org/pypi/requests/json instead",
            ),
            (
                "https://pypi.org/project/requests/2.0.0/",
                "Fetch https://pypi.org/pypi/requests/2.0.0/json instead",
            ),
            (
                "https://crates.io/crates/serde",
                "Fetch https://crates.io/api/v1/crates/serde instead",
            ),
            (
                "https://github.com/o/r/blob/main/src/lib.rs",
                "Fetch https://raw.githubusercontent.com/o/r/main/src/lib.rs instead",
            ),
            (
                "https://github.com/o/r/issues/12",
                "Fetch https://api.github.com/repos/o/r/issues/12 instead",
            ),
            (
                "https://github.com/o/r/pull/3/files",
                "Fetch https://api.github.com/repos/o/r/pulls/3 instead",
            ),
        ] {
            assert!(source(page).contains(fetch), "{page}: {}", source(page));
        }
        for page in [
            "https://stackoverflow.com/users/1",
            "https://example.com/package/x",
            "https://www.reddit.com/r/rust/",
            "https://github.com/o/r",
        ] {
            assert_eq!(source(page), "", "{page}");
        }
    }

    #[test]
    fn failures_say_what_happened_and_what_to_try() {
        let url = url::Url::parse("https://example.com/missing").unwrap();
        let headers = HeaderMap::new();
        let missing = status_failure(&url, reqwest::StatusCode::NOT_FOUND, &headers, "");
        assert!(missing.starts_with("https://example.com/missing answered HTTP 404 (Not Found): there is no page at this address."), "{missing}");
        let mut limited = HeaderMap::new();
        limited.insert(reqwest::header::RETRY_AFTER, "120".parse().unwrap());
        let api = url::Url::parse("https://api.example.com/v1").unwrap();
        let said = "{\"message\": \"API rate limit exceeded\",\n  \"documentation_url\": \"https://docs\"}";
        let limit = status_failure(&api, reqwest::StatusCode::TOO_MANY_REQUESTS, &limited, said);
        assert!(limit.contains("It asks to wait 120 seconds."), "{limit}");
        assert!(limit.ends_with("The server said: {\"message\": \"API rate limit exceeded\", \"documentation_url\": \"https://docs\"}"), "{limit}");
        let long = status_failure(
            &url,
            reqwest::StatusCode::BAD_GATEWAY,
            &headers,
            &"x".repeat(5000),
        );
        assert!(long.ends_with('…') && long.len() < 1200, "{long}");
        let blocked = bot_check_failure(
            &url::Url::parse("https://www.npmjs.com/package/react").unwrap(),
            403,
            "Cloudflare",
        );
        assert!(
            blocked.contains("https://registry.npmjs.org/react/latest"),
            "{blocked}"
        );
        assert!(
            too_large_failure(&url, 6 * 1024 * 1024, Some(14 * 1024 * 1024))
                .contains("larger than the 6.0 MB webfetch reads (it is 14.0 MB)")
        );
        assert_eq!(size(812), "812 bytes");
        assert_eq!(size(14_000), "14 KB");
        assert_eq!(size(2_215_244), "2.1 MB");
    }

    #[test]
    fn a_page_without_text_says_why() {
        let url = url::Url::parse("https://crates.io/crates/serde").unwrap();
        let note = thin_page_note(&url, 5404, "crates.io: Rust Package Registry").unwrap();
        assert!(
            note.contains("https://crates.io/api/v1/crates/serde"),
            "{note}"
        );
        assert_eq!(thin_page_note(&url, 500, "short"), None);
        assert_eq!(thin_page_note(&url, 50_000, &"word ".repeat(60)), None);
    }
}
