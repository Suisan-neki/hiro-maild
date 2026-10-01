# Implementation status (2026-10-01)

## Source and baseline verified

- Baseline `main`: `eb1443a7cc5db6480168af46209d8c84c039d59a`.
- Baseline GitHub Actions: [successful run](https://github.com/Suisan-neki/hiro-maild/actions/runs/34246031167), checked through GitHub API.
- Existing source implements Thunderbird mbox import, SQLite/attachment storage, Message-ID/SHA-256 deduplication, periodic daemon, read-only MCP and optional AI triage. Earlier README/status omitted daemon/MCP and inaccurately described CI as not run.

## Implemented in this change

- [x] Gmail API send using a personal Gmail account, addressed to that same account.
- [x] OAuth Desktop loopback/PKCE flow with account identity verification; `gmail.send` + `openid email`, no Gmail read/delete scopes.
- [x] Mac Keychain storage for client information and refresh token; no credential logging or storage in SQLite/plist/repository.
- [x] Atomic SQLite raw MIME storage and backfill of old records on local resync.
- [x] Private mbox snapshots; defer files modified during snapshot creation.
- [x] Preserve MIME body/HTML/attachments/CID/nested messages; include the complete local original as `original.eml`.
- [x] Explicit immutable start configuration: `--from-now`, `--since RFC3339`, or `--after-id`.
- [x] Persistent delivery and individual attempt history, stable forwarding Message-ID, whole-run process lock.
- [x] Backoff for known safe-to-retry failures; hold ambiguous/interrupted sends for explicit retry with duplicate-risk acknowledgment.
- [x] Network-free dry-run with target/range/MIME checks.
- [x] Opt-in forwarding in the existing daemon, including a daemon dry-run option.
- [x] Mac setup instructions and launchd plist example, initially configured for dry-run.
- [x] No university mailbox changes; no expansion of AI triage.

## Local verification

Environment: macOS ARM64 (`aarch64-apple-darwin`), Rust 1.95.0.

- `cargo check --all-targets`: passed.
- `cargo test --all-targets`: passed: 28 tests (26 unit, 1 CLI integration, 1 fixture smoke).
- `plutil -lint examples/com.suisan.hiro-maild.plist`: passed.
- Existing MCP deprecation/dead-code warnings remain unchanged.
- Tests use fictional mail, fake senders, and a loopback HTTP server with a fake token. No actual email is sent and no real account/Keychain credentials are accessed. The loopback server requires execution outside the Codex filesystem/network sandbox; test failure inside that sandbox is not skipped.

Coverage: original attachment bytes and full `.eml`, HTML/CID and nested mail, long body preservation, oversized-message hold, initial range/timezone/unknown dates/late backlog, dry-run with no queue writes, process lock, restart/resync deduplication, no-ID local-flag changes, legacy MIME backfill, retry backoff, ambiguous/crashed-send hold, OAuth callback state, HTTP payload/status/reset handling without hidden retries, and CLI operation.

## Not yet verified with real accounts or desktop integration

- [ ] Actual university Thunderbird profile paths, mailbox format, completeness of offline synchronization, and selected folders.
- [ ] Google Cloud configuration and browser consent for the user's personal Gmail.
- [ ] Real Mac Keychain permissions, especially after moving/rebuilding the executable or running via launchd.
- [ ] Refresh-token validity over time; Google External/Testing tokens usually expire after seven days for this scope set.
- [ ] Gmail acceptance/display of actual university messages, real attachment scanning, limits, spam classification, and same-account Inbox arrival.
- [ ] launchd installation, login restart, sleep/wake, and Thunderbird background synchronization on the user's Mac.
- [ ] Linux/Windows CI for this new change (baseline CI succeeded). Live OAuth/send intentionally requires Mac Keychain on this implementation.

No real-account validation is claimed. First run the documented dry-run, then manually verify one selected email including its attachments before enabling live daemon forwarding.

## Delivery and retention limitations

- Exactly-once delivery across Gmail acceptance and a lost network response/database update is impossible here. Ambiguous results are held; retrying them may duplicate mail even with the same Message-ID.
- The outer sender/date become personal Gmail/forwarding time. Original headers are kept in `.eml` and summarized in forwarding metadata; they do not authenticate the new outer message.
- `--from-now`/`--since` rely on original Date. Missing/invalid/incorrect dates may exclude mail. `--after-id` includes later-imported old mail. Review dry-run counts before sending.
- The original MIME copy increases size. Internal caps are 18 MiB original and 34 MiB composed; Gmail may impose additional restrictions.
- No Gmail read scope means automatic server-side reconciliation is unavailable. Search Gmail manually using the forwarding Message-ID when a result is unknown.
- University policy is not a grant of approval for this local method. Server-side external forwarding is not assumed or configured.
- Data, attachment files, snapshots, backups and dry-run logs contain private mail. Keep them local with appropriate permissions. Do not delete/change the delivery database to retry a mail.
- For legacy no-ID messages with no saved original, changes to flags/content before the first migration resync may prevent identity matching. A stable snapshot cannot prove that Thunderbird had already downloaded every remote MIME part; wait for its full sync.
