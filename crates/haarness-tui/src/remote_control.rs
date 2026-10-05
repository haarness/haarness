//! `/remote-control`: continue this chat from a phone or any browser through
//! Haarness Cloud. The server runs the connector and owns the state; this is
//! the dialog that shows its link and QR code.
//!
//! Every request runs on a worker task. Starting a connector waits for Cloud
//! to publish the chat, which takes seconds, and the event loop must keep
//! painting and streaming meanwhile.

use std::time::{Duration, Instant};

use anyhow::Result;
use haarness_protocol::ThreadId;
use serde::Deserialize;
use serde_json::json;

use crate::api::ApiClient;
use crate::state::{App, Overlay};

/// How often an open dialog re-asks Cloud while the connector comes up, and
/// once it is live (someone may stop sharing from the browser).
const CONNECTING_REFRESH: Duration = Duration::from_secs(2);
const LIVE_REFRESH: Duration = Duration::from_secs(10);
/// With the dialog closed, a shared chat is still re-checked now and then so
/// the band badge goes away when sharing is stopped from the browser.
const BACKGROUND_REFRESH: Duration = Duration::from_secs(60);

/// What the server says about one chat (`data` of the `remote-control` view).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Share {
    /// `signed_out`, `off`, `connecting`, `on` or `stopped`.
    pub state: String,
    pub url: Option<String>,
    pub computer: Option<String>,
    pub chat: Option<String>,
    pub error: Option<String>,
    /// Rows of `1` (dark) and `0` (light) modules.
    #[serde(default)]
    pub qr: Vec<String>,
}

impl Share {
    pub(crate) fn live(&self) -> bool {
        matches!(self.state.as_str(), "on" | "connecting")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Start,
    Stop,
    /// Ask Cloud whether it reaches the chat.
    View,
    /// Local state only; instant, used for the band badge.
    Status,
}

impl Action {
    const fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::View => "view",
            Self::Status => "status",
        }
    }
}

pub(crate) struct Request {
    pub action: Action,
    thread: ThreadId,
    receiver: tokio::sync::oneshot::Receiver<std::result::Result<Share, String>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
pub(crate) struct RemoteControl {
    /// The chat `share` describes.
    pub thread: Option<ThreadId>,
    pub share: Option<Share>,
    pub pending: Option<Request>,
    /// Why the last start or stop failed.
    pub failure: Option<String>,
    last_refresh: Option<Instant>,
    /// Sign-in started from the dialog: share as soon as it succeeds.
    pub resume_after_sign_in: bool,
}

impl RemoteControl {
    pub(crate) fn busy(&self) -> Option<Action> {
        self.pending.as_ref().map(|request| request.action)
    }

    /// Whether `thread` is reachable from Cloud, for the band badge.
    pub(crate) fn shared(&self, thread: ThreadId) -> bool {
        self.thread == Some(thread) && self.share.as_ref().is_some_and(Share::live)
    }
}

/// `/remote-control [status|stop]`.
pub(crate) fn parse(arguments: &str) -> std::result::Result<Action, String> {
    match arguments.trim().to_ascii_lowercase().as_str() {
        "" | "on" | "start" | "share" => Ok(Action::Start),
        "stop" | "off" => Ok(Action::Stop),
        "status" => Ok(Action::View),
        other => Err(format!(
            "Unknown option {other} · use /remote-control, /remote-control status or /remote-control stop"
        )),
    }
}

impl App {
    pub(crate) fn remote_control_command(
        &mut self,
        arguments: &str,
        client: &ApiClient,
    ) -> Result<()> {
        let action = parse(arguments).map_err(anyhow::Error::msg)?;
        if action == Action::Start && self.thread.is_subagent {
            anyhow::bail!(
                "Share the conversation that started this agent; its agents come with it"
            );
        }
        self.open_overlay(Overlay::RemoteControl);
        self.request_remote_control(action, client);
        Ok(())
    }

    /// Put a request on a worker task, replacing one already in flight.
    pub(crate) fn request_remote_control(&mut self, action: Action, client: &ApiClient) {
        if let Some(previous) = self.remote.pending.take() {
            // A start or stop must finish; a status read can be dropped.
            if matches!(previous.action, Action::Start | Action::Stop)
                && matches!(action, Action::View | Action::Status)
            {
                self.remote.pending = Some(previous);
                return;
            }
            previous.task.abort();
        }
        if self.remote.thread != Some(self.thread.id) {
            self.remote.thread = Some(self.thread.id);
            self.remote.share = None;
            self.remote.failure = None;
        }
        if matches!(action, Action::Start | Action::Stop) {
            self.remote.failure = None;
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let client = client.clone();
        let thread = self.thread.id;
        let task = tokio::spawn(async move {
            let result = client
                .manage(
                    "remote-control",
                    &json!({"action": action.name(), "threadId": thread}),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|view| {
                    serde_json::from_value::<Share>(view["data"].clone())
                        .map_err(|error| error.to_string())
                });
            let _ = sender.send(result);
        });
        self.remote.pending = Some(Request {
            action,
            thread,
            receiver,
            task,
        });
        self.remote.last_refresh = Some(Instant::now());
    }

    /// Frame-tick hook: take delivery of a finished request, keep an open
    /// dialog current, and learn whether a newly opened chat is shared.
    pub(crate) fn poll_remote_control(&mut self, client: &ApiClient) {
        if let Some(request) = &mut self.remote.pending {
            let result = match request.receiver.try_recv() {
                Ok(result) => result,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.remote.pending = None;
                    return;
                }
            };
            let Some(request) = self.remote.pending.take() else {
                return;
            };
            if Some(request.thread) != self.remote.thread {
                return;
            }
            let open = matches!(self.overlay, Overlay::RemoteControl);
            match result {
                Ok(share) => {
                    let was_live = self.remote.share.as_ref().is_some_and(Share::live);
                    match (request.action, share.state.as_str()) {
                        (Action::Start, "on" | "connecting") if !open => {
                            self.notice =
                                "This chat is live in Haarness Cloud · /remote-control".into();
                        }
                        (Action::Stop, _) => {
                            self.notice = share.error.clone().unwrap_or_else(|| {
                                "Stopped sharing · this chat stays on this computer".into()
                            });
                        }
                        (Action::View, "stopped") if was_live && !open => {
                            self.notice = "Sharing was stopped from Haarness Cloud".into();
                        }
                        _ => {}
                    }
                    self.remote.share = Some(share);
                }
                Err(error) => match request.action {
                    Action::Start | Action::Stop => {
                        if !open {
                            self.notice.clone_from(&error);
                        }
                        self.remote.failure = Some(error);
                    }
                    // A status read that failed changes nothing it knew.
                    Action::View | Action::Status => {}
                },
            }
            return;
        }
        if self.remote.thread != Some(self.thread.id) {
            self.request_remote_control(Action::Status, client);
            return;
        }
        let open = matches!(self.overlay, Overlay::RemoteControl);
        let interval = match self.remote.share.as_ref().map(|share| share.state.as_str()) {
            Some("connecting") if open => CONNECTING_REFRESH,
            Some("on") if open => LIVE_REFRESH,
            Some("on" | "connecting") => BACKGROUND_REFRESH,
            _ => return,
        };
        if self
            .remote
            .last_refresh
            .is_none_or(|at| at.elapsed() >= interval)
        {
            self.request_remote_control(Action::View, client);
        }
    }

    /// Enter in the dialog: the one obvious next step for its state.
    pub(crate) async fn remote_control_primary(&mut self, client: &ApiClient) -> Result<()> {
        if self
            .remote
            .busy()
            .is_some_and(|action| action != Action::Status)
        {
            return Ok(());
        }
        let state = self
            .remote
            .share
            .as_ref()
            .map_or("", |share| share.state.as_str());
        match state {
            "on" | "connecting" if self.remote.failure.is_none() => {
                self.open_remote_control_link();
            }
            "signed_out" => {
                self.remote.resume_after_sign_in = true;
                self.open_connect_picker(Some("haarness"), client).await?;
            }
            _ => self.request_remote_control(Action::Start, client),
        }
        Ok(())
    }

    pub(crate) fn stop_remote_control(&mut self, client: &ApiClient) {
        if self.remote.share.as_ref().is_some_and(Share::live)
            && self.remote.busy() != Some(Action::Stop)
        {
            self.request_remote_control(Action::Stop, client);
        }
    }

    fn remote_control_url(&self) -> Option<String> {
        self.remote
            .share
            .as_ref()
            .filter(|share| share.live())
            .and_then(|share| share.url.clone())
    }

    pub(crate) fn open_remote_control_link(&mut self) {
        if let Some(url) = self.remote_control_url() {
            self.notice = if crate::terminal::open_browser(&url) {
                "Opened the chat in your browser".into()
            } else {
                "Could not open a browser here · copy the link with c".into()
            };
        }
    }

    pub(crate) fn copy_remote_control_link(&mut self) {
        if let Some(url) = self.remote_control_url() {
            crate::terminal::copy_to_clipboard(&url);
            self.notice = "Link copied".into();
        }
    }

    /// Called when a Haarness Cloud sign-in finishes. True when the dialog
    /// asked for it and sharing has resumed.
    pub(crate) fn resume_remote_control(&mut self, client: &ApiClient) -> bool {
        if !std::mem::take(&mut self.remote.resume_after_sign_in) {
            return false;
        }
        self.open_overlay(Overlay::RemoteControl);
        self.request_remote_control(Action::Start, client);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_choose_start_status_or_stop() {
        for value in ["", "  ", "on", "start", "share", "START"] {
            assert_eq!(parse(value), Ok(Action::Start), "{value}");
        }
        assert_eq!(parse("stop"), Ok(Action::Stop));
        assert_eq!(parse("off"), Ok(Action::Stop));
        assert_eq!(parse("status"), Ok(Action::View));
        assert!(parse("later").unwrap_err().contains("/remote-control stop"));
    }

    #[test]
    fn only_a_live_share_of_the_same_chat_counts_as_shared() {
        let thread = ThreadId::new();
        let mut remote = RemoteControl {
            thread: Some(thread),
            share: Some(Share {
                state: "on".into(),
                ..Share::default()
            }),
            ..RemoteControl::default()
        };
        assert!(remote.shared(thread));
        assert!(!remote.shared(ThreadId::new()));
        remote.share.as_mut().unwrap().state = "stopped".into();
        assert!(!remote.shared(thread));
    }
}
