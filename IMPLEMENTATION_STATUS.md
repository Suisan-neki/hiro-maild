# Implementation status (2026-10-01)

## Source and baseline verified

- Baseline `main`: `eb1443a7cc5db6480168af46209d8c84c039d59a`.
- Baseline GitHub Actions: [successful run](https://github.com/Suisan-neki/hiro-maild/actions/runs/34246031167), checked through GitHub API.
- Existing source implements Thunderbird mbox import, SQLite/attachment storage, Message-ID/SHA-256 deduplication, periodic daemon, read-only MCP and optional AI triage. Earlier README/status omitted daemon/MCP and inaccurately described CI as not run.

## Implemented in this change

The user subsequently selected a **web-only** configuration and requested Render hosting preparation. The primary setup path is now the single-owner web UI described in [WEB_DEPLOYMENT.md](docs/WEB_DEPLOYMENT.md). Thunderbird/Keychain/launchd remain available as a separate local mode, using a separate data directory.

- [x] Same-origin Japanese UI: owner Gmail login, Microsoft university connection, explicit received-time start, preview, enable/pause, history and retry.
- [x] Web Authorization Code + PKCE for both providers; verified Google owner allowlist and university account identity binding.
- [x] Read-only Graph Inbox delta/MIME ingestion with immutable IDs, durable page cursor and replay deduplication. No university read-state, move, delete or send API calls.
- [x] Encrypted refresh tokens in persistent storage (ChaCha20-Poly1305, provider/email binding); client secrets/master key supplied through Render Environment.
- [x] HttpOnly/Secure host cookies, session expiry, Origin + CSRF checks, same-origin CSP; no browser-stored OAuth tokens or credential/provider-body logging.
- [x] Default-paused cloud worker; no-send preview; pause checks before each submission and interrupted-send recovery even while paused.
- [x] Render Rust Blueprint with paid persistent disk, single instance, health check, CI-gated GitHub deployment and complete OAuth setup instructions.

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
- `cargo test --all-targets`: passed: 39 tests (37 unit, 1 CLI integration, 1 fixture smoke).
- Both commands used `--locked --offline` and a temporary `CARGO_TARGET_DIR` after existing build-cache reads stalled. All required targets were checked/tested.
- `node --check web/app.js`: passed; `render.yaml` parsed as YAML.
- Browser verified: Japanese UI loads with OAuth unconfigured, transfer controls remain disabled, selecting a date enables its input.
- `plutil -lint examples/com.suisan.hiro-maild.plist`: passed.
- Existing MCP deprecation/dead-code warnings remain unchanged.
- Tests use fictional mail, fake senders, and a loopback HTTP server with a fake token. No actual email is sent and no real account/Keychain credentials are accessed. The loopback server requires execution outside the Codex filesystem/network sandbox; test failure inside that sandbox is not skipped.

Coverage: original attachment bytes and full `.eml`, HTML/CID and nested mail, long body preservation, oversized-message hold, initial range/timezone/unknown dates/late backlog, dry-run with no queue writes, process lock, restart/resync deduplication, no-ID local-flag changes, legacy MIME backfill, retry backoff, ambiguous/crashed-send hold, OAuth callback state, HTTP payload/status/reset handling without hidden retries, and CLI operation.

Web coverage: receivedDateTime independently of sender Date, from-now backlog exclusion, Graph cursor persistence/expired-state replay, repeated Graph ID and MIME deduplication, vault encryption/tamper/wrong-key/account binding, PKCE state/browser/session/provider/expiry/replay checks, verified-owner rejection, mailbox switching rejection, authenticated APIs, Origin/CSRF checks, preview requirement, pause/logout and production cookie flags. Providers are mocked; no real OAuth request or mail send is used in tests.

## Not yet verified with real accounts or desktop integration

- [ ] Actual Render service creation, billing approval, deployment, disk permissions/retention and continuous operation. No Render resources were created.
- [ ] Google Web application registration, exact public callback URL, consent and refresh-token operation.
- [ ] Microsoft app registration and university tenant consent/administrator approval, Graph MIME availability and Inbox delta behavior with the actual account.
- [ ] End-to-end browser login with actual Google/Microsoft accounts; acceptance/display/Inbox arrival of forwarded real mail.

- [ ] Actual university Thunderbird profile paths, mailbox format, completeness of offline synchronization, and selected folders.
- [ ] Google Cloud configuration and browser consent for the user's personal Gmail.
- [ ] Real Mac Keychain permissions, especially after moving/rebuilding the executable or running via launchd.
- [ ] Refresh-token validity over time; Google External/Testing tokens usually expire after seven days for this scope set.
- [ ] Gmail acceptance/display of actual university messages, real attachment scanning, limits, spam classification, and same-account Inbox arrival.
- [ ] launchd installation, login restart, sleep/wake, and Thunderbird background synchronization on the user's Mac.
- [ ] Linux/Windows CI for the web addition (baseline and prior local-forwarding CI succeeded). Web OAuth/send uses encrypted server-side credentials on Linux; only the native local CLI requires Mac Keychain.

No real-account validation or deployment is claimed. Follow Web setup, inspect targets while paused, and manually verify the first received mail and attachments. Mac/Thunderbird startup is not required for the chosen web mode.

## Delivery and retention limitations

- Exactly-once delivery across Gmail acceptance and a lost network response/database update is impossible here. Ambiguous results are held; retrying them may duplicate mail even with the same Message-ID.
- The outer sender/date become personal Gmail/forwarding time. Original headers are kept in `.eml` and summarized in forwarding metadata; they do not authenticate the new outer message.
- Web start uses Graph receivedDateTime and reads Inbox only; server rules that deliver to other folders or pre-import moves/deletes can exclude messages. Local CLI `--from-now`/`--since` rely on original Date; `--after-id` includes later-imported old mail.
- The original MIME copy increases size. Internal caps are 18 MiB original and 34 MiB composed; Gmail may impose additional restrictions.
- No Gmail read scope means automatic server-side reconciliation is unavailable. Search Gmail manually using the forwarding Message-ID when a result is unknown.
- University policy is not a grant of approval for this application or external storage. Server-side external forwarding is not assumed or configured.
- Web mode stores mail and attachments on Render; only authentication tokens are app-encrypted. Local mode stores mail on the Mac. Protect data/backups and retain the master key separately from the web database. Do not delete/change the delivery database to retry a mail.
- For legacy no-ID messages with no saved original, changes to flags/content before the first migration resync may prevent identity matching. A stable snapshot cannot prove that Thunderbird had already downloaded every remote MIME part; wait for its full sync.
