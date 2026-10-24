# Desktop streaming Markdown

How Desktop renders a reply's Markdown incrementally as it streams, keeps it sanitized, and how that is tested. For contributors working on `renderer/Markdown.tsx`, `markdown-project.ts`, `markdown-client.ts` and `markdown.worker.ts`; there is no user guide for this internal behaviour.

Desktop keeps completed Markdown blocks mounted while a reply grows. A shared module worker lexes the changing tail and returns HTML patches; the renderer sanitizes those patches and updates existing elements and text nodes. Appending text does not replace the whole answer or clear a selection in unchanged text. Streaming reasoning uses the same path.

The parser retains two trailing top-level blocks because partial syntax can merge with the preceding block, such as a new list item. Completed HTML has a shared cache bounded to 256 entries and 2 MiB. Reference-definition syntax triggers a whole-document parse so a definition arriving later can resolve an earlier link. Final rendering also checks the complete document. Incomplete fences, lists, tables and headings follow the existing Marked parser rather than a separate Markdown dialect. Replies keep single line breaks (Marked's `breaks` option), as the TUI does; a Markdown file preview keeps standard paragraphs, where a lone newline is a space. The cache keys blocks by mode.

Each mounted message owns its worker projection. Requests coalesce while parsing; superseded replies update transport state without rendering obsolete text. Unmounting releases the projection and ignores late responses. The idle worker terminates after 30 seconds without clients. Worker startup, transport or timeout failure falls back to the same incremental parser locally. Parsing moves off the UI thread in normal operation; sanitization and DOM updates remain on it. Very large individual blocks and documents containing reference definitions can still require substantial parsing.

Model HTML remains untrusted. Sanitization precedes every DOM update, including fallback, and uses a DOMPurify instance of its own (`markdown-dom.ts`), so a hook or setting another part of the app adds to the shared instance never applies to replies. Model-authored scripts, event handlers, form controls and inline styles remain excluded. An image from another host becomes a click-to-load placeholder in the same pass (see [Desktop](DESKTOP.md)), so a later patch of the block reproduces the placeholder, or the image once it is loaded, rather than undoing the click. In the conversation an image whose source is a path becomes, in that pass too, the project file's image once read or a placeholder while it is read, so a patch never puts back a path for the page to load. Code-copy buttons appear only after streaming finishes; links still use Desktop's external-link handler.

Before drawing, the tokens of every block pass through one step (`replyToken` in `markdown-project.ts`, also given to `marked.parse` as `walkTokens`, so streamed and whole-document output stay equal). Inside a paragraph, a list item or a table cell, a bare tag that is not an HTML element showing its text in place (`<path>`, `<text>`, `<file>`, `<svg>`, `<title>`, `<App />`) is shown as the text it is, as the TUI shows it, together with its closing tag in the same run of text. Read as markup, the sanitizer removed an SVG name outside an svg with everything after it in its block, and `<svg>` or `<title>` hid what followed. A block of HTML that starts with such a tag (`<path>` alone on a line, `1. <file>`, a line starting `<title>`, `<head>` or `<form>`) is read as a paragraph of the reply, or a tight list item's text, in the same way. Tags with attributes, elements such as `<kbd>`, `<br>` or a `<ul>` in a table cell, and block HTML starting with such an element (`<details>`, `<div>`, `<table>`) or a comment stay markup, so an inline SVG drawing still draws, and so does a bare `<svg>` or `<math>` wrapping elements of its own (`<svg><path d=…/></svg>`, `<math><mi>x</mi></math>`). A link inside a link's label (`[<dev@example.com>](url)`, `[[a](x)](y)`) keeps only its text, so the label goes where its link points instead of being split into an empty link and the inner one.

## Verification

From `apps/desktop`:

```sh
npm test
npm run typecheck
npm run build
npm run test:e2e -- --grep 'incremental Markdown|streaming respects upward|conversation find reaches'
```

Tests compare streaming output against whole-document Marked output at every character boundary, in both modes, including mixed block sequences and late reference definitions. Transport tests cover coalescing, stale replies, owner disposal, recovery, and idle worker teardown.

Electron tests use a real isolated Haarness daemon and a controlled HTTP streaming provider. They cover a 500-paragraph reply with 40 tail updates, selected-text and node retention, worker loading from the production bundle, incomplete/completed fences, tables, reference links, HTML sanitization, code copying, reload, and worker-failure recovery. Existing scrolling and conversation-search tests cover interaction with the transcript.

On September 13, 2026, the long-reply probe retained its selected text with zero removed DOM nodes and no observed long tasks during the 40 updates. A separate parser-only comparison over 200 updates to a 43,394-character document took 30–50 ms incrementally versus 817–1,151 ms for repeated full parsing across five runs. These are local fixture measurements, not a general speed guarantee or an end-to-end rendering speedup.

All 108 Desktop unit tests, type checking, the production build, and the five focused Electron workflows passed. The broader `SSE chat fabric` test missed its Send guidance button during a short timed response; the same failure reproduced with the original renderer.
