# 実装状況（2026-10-01）

## 現在選択している構成

ユーザーの最新の希望は「無料で、1日2回程度ブラウザーを開いて同期する」です。**GitHub Pagesの静的UIと、ブラウザーで実行する手動同期**を主な運用として実装しました。Render・有料ディスク・常時worker・GitHub Actionsでの定期送信は使いません。[設定・使い方](docs/WEB_DEPLOYMENT.md)

GitHub Actionsはビルド、架空メールでのテスト、静的サイト公開のみです。公開コードにclient secret、トークン、メールは含みません。OAuthアプリ登録・大学側の同意・実アカウントでの確認は別途必要です。

## 実装済み

- 日本語UI：Google/大学Microsoftログイン、明示的な開始日時、送信なしの対象確認、手動同期・転送、停止、履歴、再試行の予約。
- Google Identity Services token model（gmail.send + openid/email）、確認済み個人Gmailの照合。client secret・サーバー側refresh token不要。
- Microsoft MSAL Browser 5：SPA Authorization Code + PKCE、専用redirect bridge、User.Read/Mail.Read、大学ドメインとGraphアカウントIDの確認。
- トークンは画面のメモリだけ。MSALのログイン中のstate/PKCE等にはsessionStorageを使用。公開client IDはIndexedDB。認証情報・メール・認証応答はログに出さない。
- Read-only Graph Inbox delta / MIME取得。受信日時での開始範囲、immutable ID、ページごとの位置確定、期限切れからの再取得。大学側の既読化・移動・削除・送信APIなし。
- IndexedDBの転送履歴・重複キー・Graph ID別名・転送待ち原本。Message-ID、なければSHA-256による重複排除。開始日時と両アカウントは固定。
- RustのMIME組立処理を`crates/mail-core`へ分離し、従来CLIとWebAssemblyで共用。本文・HTML・添付・CID・入れ子メールと全原本original.emlを保持。
- Web Locksによるタブ間排他。Gmail POST前の送信意図の確定。成功後に小さな送信履歴を残し、原本を解放。
- 429バックオフ（次の手動同期で再試行）、拒否時の保留、通信断/不明応答/中断/成功後の保存失敗を結果不明として保留。結果不明はGmail確認と重複リスクの了承後だけ再試行。
- Pages公開workflow、画面内の登録ガイド・データ取扱説明、詳細な設定手順。有料Render設定は`examples/render-paid.yaml`へ移し、任意の別構成として扱う。

以前の実装も維持しています：Thunderbirdの非公開mboxスナップショット、SQLiteへの全原本・本文・添付保存と原本の再同期補完、Gmail Desktop OAuth + Mac Keychain、CLIの開始位置/dry-run/送信履歴/バックオフ/不明結果の保留、daemonとの任意連携、read-only MCPとlaunchd手順。AIトリアージは今回拡張せず、自動実行にも追加していません。以前のRustサーバーWeb OAuth/暗号化vault/workerも[別構成の参考](docs/SERVER_DEPLOYMENT.md)として残します。

## 検証

- `cargo check --all-targets --locked --offline`：成功。
- `cargo test --all-targets --locked --offline`：成功、39件（37 unit + 2 integration）。
- `cargo test -p hiro-mail-core --locked --offline`：成功、3件。
- `npm run build`：Rust/WASMおよび静的本番サイトのビルド成功。
- `npm test`：成功、19件。中断・不正MIME・成功後保存失敗も検証。
- 本番ビルドをMacのブラウザーで確認：初期設定案内、未接続時の送信禁止、日時指定、空client IDの拒否。実アカウントのログインは行っていません。
- ローカルはmacOS ARM64 / Rust 1.95.0 / Node 24.10.0。既存のビルド領域の読み取り停滞を避け、Rustは`/tmp/hiro-maild-web-target`を使用。既存MCPの非推奨/dead-code警告は残ります。

テストは架空メール・偽API・偽トークンだけを使用し、実メールは送信しません。RustのHTTPテストはloopbackモックを使います。ブラウザーテストには実WASMとfake IndexedDBを使い、添付/CID/入れ子メール/原本バイト、受信日時での初回範囲、送信なし確認、再開・再取得・別Graph IDの重複防止、429再試行、結果不明の保留、ページ失敗と期限切れからの再開、タブ排他、送信前保存失敗、上限超過を検証しています。

GitHub PagesはActions方式で有効化済み。サイト本体の公開workflowはこれから実行します。以前のWebサーバー追加コミットはLinux/macOS/Windowsのcheck/testが成功しています。最終変更のCIと公開の結果は完了後に更新します。

## 実アカウントで未確認

- Google Web applicationの登録、origin登録、テストユーザー、実際のpopup同意とトークン取得。
- Microsoft SPAアプリ登録、redirect bridgeによる実際のログイン、大学テナントでの同意/管理者承認可否。
- 実際の大学Inbox/Graph delta/MIMEとブラウザーCORS通信。
- 個人Gmailへの受信、HTML/CID/添付表示、添付検査、迷惑メール判定、same-account Inboxへの到着。
- ユーザーが普段使うブラウザーでの保存容量・退避/削除・Macスリープや通信中断の挙動。
- 従来ローカル方式の実Thunderbirdプロフィール、Keychain、launchd自動起動（今回の運用では不要）。

ログインできること・大学側が承認すること・実メールが届くことは、モック検証だけでは保証していません。アプリ登録後、送信なしで対象を確認し、最初の少量をGmailで手動照合してください。

## 制限

- 同じブラウザー/プロファイル/originだけで履歴を保持。別端末やデータ削除後には引き継げず、履歴の消失/巻き戻しで重複送信の可能性があります。端末間共有・バックアップは未実装。
- 閉じた画面の処理は継続しません。受信から次の手動同期までGmailに届きません。
- Inboxのみ。同期前にInboxから移動・削除されたメールは取得できません。
- 原本18 MiB、組立後34 MiBの上限。添付を削って送信せず保留します。
- 外側のFrom/Toは個人Gmail、Dateは転送時刻。元送信者・日時は本文と原本内に保持。元署名/DKIMが外側メールに有効なわけではありません。
- Gmail APIにexactly-once保証はなく、送信結果が不明なメールの再試行には重複リスクがあります。自動再送・Gmail読取照合はしません。
- 大学の[公式注意事項](https://www.media.hiroshima-u.ac.jp/services/hirodaimail/notice/)を確認済み。サーバー側外部自動転送は前提にしません。本方式や外部保管が大学から承認済みという意味ではありません。

## 元の状態とレビュー

baseline main：`eb1443a7cc5db6480168af46209d8c84c039d59a`。[baseline CI成功](https://github.com/Suisan-neki/hiro-maild/actions/runs/34246031167)をGitHub APIで確認済み。既存ソースにmbox import、daemon、read-only MCP、任意AIトリアージがあり、以前のREADME/状態記述を修正しました。

レビュー用ブランチ：`codex/gmail-forwarding`。[PR #1](https://github.com/Suisan-neki/hiro-maild/pull/1)。以前のローカル転送実装`ad22c8e`、Rustサーバー版`a2d0db0`に続け、無料ブラウザー手動同期を追加しています。mainへのマージは行っていません。
