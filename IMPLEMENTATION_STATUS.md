# Implementation status

## Implemented

- [x] Cross-platform Thunderbird profile discovery
- [x] Hiroshima University account selection from Thunderbird `prefs.js`
- [x] Reject unrelated Outlook accounts during automatic discovery
- [x] Read-only mbox ingestion
- [x] Message-ID / SHA-256 deduplication
- [x] SQLite schema and transactional message/attachment import
- [x] MIME parsing
- [x] Attachment filename sanitization
- [x] Attachment extraction size limits
- [x] PDF text extraction
- [x] DOCX/PPTX/XLSX text extraction
- [x] JSON Lines listing
- [x] OpenAI structured triage client
- [x] `store: false` on AI requests
- [x] Dry-run mode before any email content leaves the machine
- [x] Unit/smoke test sources and mbox fixture
- [x] GitHub Actions CI for Linux/macOS/Windows

## Next validation

- [ ] GitHub Actions: run `cargo check` / `cargo test` on Linux, macOS, and Windows after source push.
- [ ] Real Thunderbird integration test: requires the user's locally synchronized Thunderbird profile.
- [ ] Real OpenAI triage call: requires the user's API key and explicit decision to send mail content to the API.
