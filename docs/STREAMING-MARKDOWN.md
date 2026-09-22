# Desktop streaming Markdown

Desktop keeps completed Markdown blocks mounted while a reply grows. A shared module worker lexes the changing tail and returns HTML patches; the renderer sanitizes those patches and updates existing elements and text nodes. Appending text does not replace the whole answer or clear a selection in unchanged text. Streaming reasoning uses the same path.

The parser retains two trailing top-level blocks because partial syntax can merge with the preceding block, such as a new list item. Completed HTML has a shared cache bounded to 256 entries and 2 MiB. Reference-definition syntax triggers a whole-document parse so a definition arriving later can resolve an earlier link. Final rendering also checks the complete document. Incomplete fences, lists, tables and headings follow the existing Marked parser rather than a separate Markdown dialect.

Each mounted message owns its worker projection. Requests coalesce while parsing; superseded replies update transport state without rendering obsolete text. Unmounting releases the projection and ignores late responses. The idle worker terminates after 30 seconds without clients. Worker startup, transport or timeout failure falls back to the same incremental parser locally. Parsing moves off the UI thread in normal operation; sanitization and DOM updates remain on it. Very large individual blocks and documents containing reference definitions can still require substantial parsing.

Model HTML remains untrusted. Sanitization precedes every DOM update, including fallback. Model-authored scripts, event handlers, form controls and inline styles remain excluded. Code-copy buttons appear only after streaming finishes; links still use Desktop's external-link handler.

## Verification

From `apps/desktop`:

```sh
npm test
npm run typecheck
npm run build
npm run test:e2e -- --grep 'incremental Markdown|streaming respects upward|conversation find reaches'
```

Tests compare streaming output against whole-document Marked output at every character boundary, including mixed block sequences and late reference definitions. Transport tests cover coalescing, stale replies, owner disposal, recovery, and idle worker teardown.

Electron tests use a real isolated Haarness daemon and a controlled HTTP streaming provider. They cover a 500-paragraph reply with 40 tail updates, selected-text and node retention, worker loading from the production bundle, incomplete/completed fences, tables, reference links, HTML sanitization, code copying, reload, and worker-failure recovery. Existing scrolling and conversation-search tests cover interaction with the transcript.

On September 13, 2026, the long-reply probe retained its selected text with zero removed DOM nodes and no observed long tasks during the 40 updates. A separate parser-only comparison over 200 updates to a 43,394-character document took 30–50 ms incrementally versus 817–1,151 ms for repeated full parsing across five runs. These are local fixture measurements, not a general speed guarantee or an end-to-end rendering speedup.

All 108 Desktop unit tests, type checking, the production build, and the five focused Electron workflows passed. The broader `SSE chat fabric` test missed its Send guidance button during a short timed response; the same failure reproduced with the original renderer.
