//! What a person accepts before agents get this Mac's screen. Desktop translates each sentence (the English text is
//! its key in `locales/*.json`); the CLI and terminal UI print the English. Any change in substance needs a new
//! [`VERSION`], which makes everyone accept again before agents can use the screen.

use serde_json::{Value, json};

pub const VERSION: &str = "1";

pub const TITLE: &str = "Terms for computer use";

pub const INTRO: &str = "Turning on computer use gives Haarness agents control of this Mac much like your own: they capture the screen, read app contents via macOS Accessibility, and use the mouse and keyboard. By switching it on you accept every point below.";

pub const FOR: &[&str] = &[
    "Developing, testing, troubleshooting and showing off software made by you or your team.",
    "Automating everyday chores in apps, on a Mac and with data you have the right to use.",
    "Recreating bugs and grabbing screenshots for reports, provided they contain no personal data.",
];

pub const NOT_FOR: &[&str] = &[
    "Looking into, gathering, copying or sharing anybody else's personal information, from contact details and private conversations to health, money or ID records, pictures and whereabouts. Close anything like that before an agent begins, and never point an agent at it.",
    "Handling secrets of any kind: passwords, PINs, one-time codes, API keys, tokens, card numbers. Password fields are off limits to agents and password managers are invisible to them, so you sign in yourself.",
    "Carrying confidential information into apps, accounts or services it shouldn't reach, or touching more of it than the job requires.",
    "Letting an agent send messages or posts, agree to terms, spend or move money, trade, or alter settings for accounts, security or privacy without you reviewing each such step.",
    "Defeating security measures, CAPTCHAs or monitoring, or breaking an app's or service's rules.",
    "Operating any Mac, account or session that isn't yours to use.",
];

pub const UNDERSTAND: &[&str] = &[
    "Whatever is visible while an agent works, notifications and unrelated windows too, may be sent to your chat's AI model and is kept in the chat history.",
    "App contents can't be trusted: a web page, file or message might carry instructions aimed at the agent. Keep an eye on it and press Stop if anything seems off.",
    "An agent asks before acting in an app you haven't allowed. Once you allow an app it acts there without asking, and what it does remains your responsibility.",
    "macOS grants Accessibility and Screen Recording to whichever app launched Haarness, either Haarness itself or your terminal, and every program launched from that app inherits them.",
    "Switch computer use off whenever you like under Settings → Computer use, or run haarness mac off.",
];

/// Every sentence of the terms in reading order, for clients that translate them one by one.
#[must_use]
pub fn sentences() -> Vec<&'static str> {
    let mut all = vec![TITLE, INTRO];
    all.extend(FOR);
    all.extend(NOT_FOR);
    all.extend(UNDERSTAND);
    all
}

/// The terms as data: what Desktop renders.
#[must_use]
pub fn json() -> Value {
    json!({
        "version": VERSION,
        "title": TITLE,
        "intro": INTRO,
        "sections": [
            {"heading": "Use it for", "items": FOR},
            {"heading": "Don't use it for", "items": NOT_FOR},
            {"heading": "Keep in mind", "items": UNDERSTAND},
        ],
    })
}

/// Plain-text rendering, printed by `haarness mac` and shown by `/mac`.
#[must_use]
pub fn text() -> String {
    let list = |items: &[&str]| {
        items
            .iter()
            .map(|item| format!("  • {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    [
        TITLE.to_owned(),
        String::new(),
        INTRO.to_owned(),
        String::new(),
        "Use it for:".to_owned(),
        list(FOR),
        String::new(),
        "Don't use it for:".to_owned(),
        list(NOT_FOR),
        String::new(),
        "Keep in mind:".to_owned(),
        list(UNDERSTAND),
    ]
    .join("\n")
}
