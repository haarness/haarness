//! Log lines a test can read back, to check what the daemon log says of a
//! request and what it leaves out.

use std::fmt::Write as _;
use std::sync::{Arc, LazyLock, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Each event as `LEVEL message | name=value …`.
#[derive(Clone, Default)]
pub(crate) struct Logged(Arc<Mutex<Vec<String>>>);

/// tracing remembers whether a log statement is wanted from the dispatchers
/// registered when it first runs. A test thread logging with none would
/// leave a statement switched off for every other test; one dispatcher that
/// always listens, registered for good, keeps every statement asking.
static ALWAYS_LISTENING: LazyLock<tracing::Dispatch> =
    LazyLock::new(|| tracing::Dispatch::new(Logged::default()));

impl Logged {
    /// Records what this thread logs until the guard is dropped; a tokio
    /// test's runtime runs on the same thread, its spawned tasks included.
    pub(crate) fn start() -> (Self, tracing::subscriber::DefaultGuard) {
        LazyLock::force(&ALWAYS_LISTENING);
        let logged = Self::default();
        let guard = tracing::subscriber::set_default(logged.clone());
        (logged, guard)
    }

    /// The lines whose message is `message`.
    pub(crate) fn lines(&self, message: &str) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|line| {
                line.split_once(' ')
                    .is_some_and(|(_, rest)| rest.split(" |").next() == Some(message))
            })
            .cloned()
            .collect()
    }

    /// Everything logged so far, one line an event.
    pub(crate) fn all(&self) -> String {
        self.0.lock().unwrap().join("\n")
    }
}

#[derive(Default)]
struct Line {
    message: String,
    fields: String,
}

impl Visit for Line {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
}

impl Subscriber for Logged {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut line = Line::default();
        event.record(&mut line);
        self.0.lock().unwrap().push(format!(
            "{} {} |{}",
            event.metadata().level(),
            line.message,
            line.fields
        ));
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}
