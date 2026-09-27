---
name: haarness-files-uploads
description: Test Haarness file explorer, hidden runtime configuration, file uploads/downloads and agent access through session API, SDK and Chrome UI.
---

# Workspace files and uploads

Read Cloud `docs/WORKSPACE-SETUP-VERIFICATION-2026-09-27.md`, native `sdks/tests/files_binary.py`, and the file explorer API definitions. Use a disposable session and owned marker files.

Create an independent empty session and a template-backed session. The explorer must show the initialized `.haarness` workspace/configuration, including attached skills, agents, MCPs and plugin files. Hidden files are always visible; do not add a hide-dotfiles toggle. Check `.git` and nested dotfiles where they exist.

In Chrome control: expand folders, preview text, create/edit/rename/delete owned files, upload text and binary files with spaces/Unicode names, and download them. Verify zero-byte files, binary byte identity, MIME/content handling, traversal/symlink rejection and permission errors. API success does not prove the browser picker worked; inspect its visible outcome.

Run native `sdks/tests/files_binary.py` and relevant Cloud workspace setup/explorer tests. Compare API/SDK bytes with the originals; never base binary equality on a decoded text preview. Refresh/reconnect must preserve selection, expansion and unsaved edits where supported. Check changes arrive via runtime events without refreshing the whole page.

Ask the LLM to read an uploaded marker file and summarize a known fact only present inside it. Ask a second chat in the same session to read it: it shares the filesystem, while conversation history remains separate. Verify a different independent session cannot read that file. Load/unload a skill/plugin and inspect its config files and actual LLM behavior.

Stop/restart and confirm intended persistence. Delete the test session and verify worker cleanup of its files and chats. Retain evidence outside the disposable workspace and remove only owned test files.
