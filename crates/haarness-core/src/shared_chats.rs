//! The chats that published-chat viewers reach on this server. A published chat shows its transcript to people other
//! than the owner, so a tool that reveals this computer beyond the project (the screen, for one) stays out of it. The
//! server learns of one when a viewer's request arrives, and the mark lasts as long as the server runs: a chat
//! unpublished since then keeps it until the next start.

use haarness_protocol::ThreadId;
use std::{collections::HashSet, sync::LazyLock};

static SHARED: LazyLock<parking_lot::RwLock<HashSet<ThreadId>>> = LazyLock::new(Default::default);

/// Records that viewers of the published chat `root` reach it and the chats below it.
pub fn mark(root: ThreadId) {
    if !SHARED.read().contains(&root) {
        SHARED.write().insert(root);
    }
}

/// Whether `thread` is a published chat whose viewers came here, or (given its conversation's root) lies below one.
#[must_use]
pub fn contains(thread: ThreadId, root: ThreadId) -> bool {
    let shared = SHARED.read();
    shared.contains(&thread) || shared.contains(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_published_chat_and_the_chats_below_it_are_shared() {
        let root = ThreadId::new();
        let child = ThreadId::new();
        let other = ThreadId::new();
        assert!(!contains(child, root));
        mark(root);
        assert!(contains(root, root));
        assert!(contains(child, root));
        assert!(!contains(other, other));
    }
}
