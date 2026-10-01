# hiro-maild

広島大学のメールを、本文・添付込みで個人Gmailへ転送します。**現在の運用は、MacのThunderbirdで受信し、Mac上のブラウザ画面で対象を確認して手動転送する構成です。** 有料サーバー・Render・永続ディスク契約は不要です。ThunderbirdとローカルのRustプログラムを起動して使います。

**[Macの画面・設定手順](docs/LOCAL_WEB.md)** · [実装済み/未確認の範囲](IMPLEMENTATION_STATUS.md) · [大学APIを使うPages版の手順](docs/WEB_DEPLOYMENT.md)

`hiro-maild local-web`で表示される `http://127.0.0.1:8082/` を開き、Thunderbirdの保存先・個人Gmail・転送開始位置を設定します。GoogleのDesktop app OAuthで認証し、資格情報はMacのキーチェーン、メールと履歴はSQLiteへ保存します。送信なしの確認はGoogle認証前でも利用できます。Microsoftの独自アプリ登録は不要です。

GitHub Pagesだけで動くGraph版も残していますが、2026-10-01の大学アカウントでのログインは **AADSTS90094（管理者承認が必要）** で停止しました。個人Azureにアプリを作っても大学の同意ポリシーは変わりません。現在選択したローカル画面は大学APIを使用しません。Thunderbirdの実同期とGmailの実認証・受信は別途確認が必要です。

大学Inboxは読み取りだけで、既読化・移動・削除・返信を行いません。Gmail転送は認証した個人Gmailから同じGmailへ新しいメールを送る処理です。初回の大量転送防止、再同期の重複排除、履歴、失敗の再試行、結果不明時の保留を実装しています。

従来のThunderbirdローカルmbox方式、read-only MCP、任意実行のAIトリアージもCLIに残っています。今回AI機能は拡張していません。以下のMac/daemon手順は**ローカル方式向け**です。以前用意した[有料サーバー版](docs/SERVER_DEPLOYMENT.md)も参考として残しますが、今回の無料手動同期では使用しません。

## 公式案内と認証方式

2026-10-01に確認した[大学の注意事項](https://www.media.hiroshima-u.ac.jp/services/hirodaimail/notice/)では、Microsoft 365サーバーの外部自動転送を設定しないよう案内されています。サーバー側の転送設定は使用しません。このアプリの方式が大学から承認されたという意味ではありません。同ページが挙げる外部保管の情報漏洩、着信制限、喪失の懸念は本アプリでも考慮が必要です。

Gmail APIの[users.messages.send](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.messages/send)を使い、OAuth Desktop appの[loopback + PKCE認証](https://developers.google.com/identity/protocols/oauth2/native-app)を実装しています。メールの権限は`gmail.send`のみ。アカウントの照合用に`openid email`も要求し、Gmailの読取・削除権限は要求しません。通常のGoogleパスワードやSMTPアプリパスワードは使いません。Googleは[アプリパスワードよりGoogleによるログインを推奨](https://support.google.com/mail/answer/185833?hl=en)しています。

ローカルCLIの実送信と認証はmacOSのキーチェーン対応です。無料ブラウザー版はGoogleのtoken modelとMicrosoftのSPA PKCEを使い、トークンは画面のメモリで扱います。ブラウザー版の認証方式と権限は[設定手順](docs/WEB_DEPLOYMENT.md)を参照してください。

## Gmailで何が保持されるか

| 項目 | 転送後の扱い |
|---|---|
| 外側のFrom / To | 認証した個人Gmail。元の送信者を装わない |
| 外側のDate | 転送メールを組み立てた時刻。Gmailでの受信時刻は転送時刻 |
| 件名 | `[広大メール]`を付ける。UnicodeをRFC2047でエンコード |
| 元送信者・元日時・元件名・元Message-ID | 本文先頭の転送情報に表示。元ヘッダーそのものは`.eml`に保持 |
| 本文・HTML・添付・インライン画像 | 元のContent-*ヘッダーとMIME本文を入れ子にしてコピー。文字コード・Content-ID・添付名・バイナリ内容を維持 |
| 原本 | `original.eml`を追加添付。mboxパーサーが取り出したメール全体、またはGraphの`/$value`が返した全MIMEをバイト単位で保存 |
| 署名、暗号化、DKIM | 原本内には残るが、外側の転送メールの署名として有効ではない。暗号化を解除する機能はない |

ブラウザー版は転送待ち・保留中の原本をIndexedDBに保存し、成功後は原本を解放して送信履歴を残します。ローカル/サーバー版の原本はSQLiteの`raw_messages.mime`（BLOB）に保存します。本文の一覧用テキストは従来どおり200,000文字までですが、転送にはその切り詰めたテキストを使いません。元の入れ子メール添付もMIME内に保持します。Thunderbirdのmbox区切り行・エスケープ解除などを経たローカル原本であり、受信サーバー上のwire bytesとの同一性は保証しません。Gmailによる表示・再エンコード・添付検査は実アカウントで未検証です。

原本`.eml`の追加でサイズが増えます。元MIMEは18 MiB、組立後は34 MiBという内部上限を設けています（両方を満たす必要があります）。不足・不正なMIMEや上限超過は`blocked`になり、添付を黙って落として送信しません。

## Macで設定して起動する

以下の`you@gmail.com`とThunderbirdストアのパスを、自分の値に置き換えてください。同じデータディレクトリをすべてのコマンドで使用してください。履歴を失うと再送を防げません。

### 1. ビルドとローカル同期

```bash
cargo build --release --locked
export HIRO_MAILD_DATA_DIR="$HOME/Library/Application Support/hiro-maild"
export HIRO_MAILD_THUNDERBIRD_STORE="$HOME/Library/Thunderbird/Profiles/xxxx.default/ImapMail/outlook.office365.com"
mkdir -p "$HIRO_MAILD_DATA_DIR"
chmod 700 "$HIRO_MAILD_DATA_DIR"
./target/release/hiro-maild doctor
./target/release/hiro-maild sync
./target/release/hiro-maild list --limit 10
```

Thunderbirdを大学の公式案内どおりIMAP/OAuth2で設定し、「同期とディスク領域」で対象フォルダーの**本文と添付を含むメッセージ全体**をローカル同期してください。maildirには未対応です。`.msf`に対応する拡張子なしmboxを読みます。各ファイルの非公開一時スナップショットを作り、コピー中にサイズ/更新時刻が変わったファイルは次回へ延期します。一時領域には最大mbox相当の空き容量が必要です。指定ストア内の全対象mbox（送信済み、ゴミ箱なども含む）が取込対象なので、不要なフォルダーを同期対象から外すか、対象フォルダーだけを含む読み取り用コピーを別ストアとして指定してください。

旧バージョンのDBには原本MIMEがありません。同じThunderbirdストアを`sync`すると、既存IDを保持して原本を補完します。元メールがローカルに残っていない場合は再構築しません。

### 2. 転送開始位置を明示する

推奨は、初回同期後に「今から」を設定することです。このコマンドは送信しません。

```bash
./target/release/hiro-maild forward-init --gmail you@gmail.com --from-now
./target/release/hiro-maild forward --dry-run --limit 100
```

開始方法は必ず一つだけ選びます。

- `--from-now`: 現在の最大取込IDより後、かつ元のDateが設定時刻以降のメール。後から同期された過去メールも除外するため、大量転送を防ぐ既定の手順です。
- `--since '2026-10-01T09:00:00+09:00'`: 元のDateが指定時刻以降のメールを、既存取込済み分も含めて対象にします。異なるタイムゾーンも同じ時刻として比較します。
- `--after-id 123`: ローカル取込IDが123より大きいメール。元のDateは問いません。後から初めて取り込む過去メールも対象になります。`--after-id 0`は過去分を全件含める明示的な選択です。

日時指定では欠損・不正なDateは対象外です。送信者の時計がずれている場合も除外され得ます。`forward --dry-run`の範囲集計で除外数と日時不明数を確認してください。開始設定と宛先はDBに一度だけ保存し、再起動しても変わりません。範囲変更用のリセット機能はありません。履歴の破棄や別データディレクトリへの変更は、新しい転送として重複送信につながり得ます。

### 3. 自分のMacでGmailを認証する

1. [Google Cloud Console](https://console.cloud.google.com/)で自分用のプロジェクトを作り、Gmail APIを有効にします。
2. Google Auth PlatformでAudienceをExternalに設定し、Testingの場合は自分の個人Gmailをテストユーザーに追加します。Data Accessで`gmail.send`、`openid`、`email`を設定します。
3. OAuth clientを**Desktop app**として作成し、JSONをダウンロードします。リポジトリ・同期共有フォルダーの外に置き、権限を600にします。
4. 次のコマンドを実行し、表示されたURLを**同じMacのブラウザー**で開いて、指定した個人Gmailで同意します。loopback callbackは5分で終了します。認証だけではメールを送信しません。

```bash
chmod 600 "$HOME/Downloads/hiro-maild-oauth.json"
./target/release/hiro-maild gmail-auth \
  --gmail you@gmail.com \
  --client-json "$HOME/Downloads/hiro-maild-oauth.json"
```

クライアント情報とrefresh tokenは、ログインキーチェーンのservice `hiro-maild.gmail`、account `you@gmail.com`に保存します。access tokenはメモリのみです。CLI引数・DB・plist・ログにトークンやclient secretを保存しません。ダウンロードしたJSONはユーザー管理の入力ファイルであり、アプリはリポジトリへコピーしません。キーチェーンアクセスの許可がMacに表示されたら、そのバイナリに対して設定してください。再ビルド・配置変更で再確認されることがあります。

External / Testingのrefresh tokenは、今回のスコープ構成では[通常7日で期限切れ](https://developers.google.com/identity/protocols/oauth2#expiration)になります。常用時は自分用アプリの公開状態・Googleの検証要件を確認してください。Testingのままなら期限切れ時に`gmail-auth`を再実行します。認可取消しなどでも再認証が必要です。

### 4. dry-runを確認してから実行する

```bash
./target/release/hiro-maild forward --dry-run --limit 100
# 内容・範囲を確認後、最大1件で最初の実アカウント検証
./target/release/hiro-maild forward --limit 1
./target/release/hiro-maild forward-status --limit 100
# 確認後に定期実行
./target/release/hiro-maild daemon --forward --interval-seconds 300
```

`forward`はDBから送るだけで、同期しません。daemonは起動時と各周期で同期してから転送します。dry-runはOAuth認証・ネットワーク通信・転送キュー/履歴への書き込みをせず、今回試行可能な候補のID、件名、元日時、添付名、原本の有無、送信状態、MIME検証結果を表示します。保留中・送信済みの履歴は`forward-status`で確認します。DBを開くためのスキーマ初期化は行います。`--limit`は表示件数/各周期の最大試行数です（転送は1〜200）。未設定なら実行を拒否します。

定期実行も先に試せます:

```bash
./target/release/hiro-maild daemon --forward --forward-dry-run --interval-seconds 300
```

MCPは従来どおり`http://127.0.0.1:8000/mcp`のread-only endpointです。daemonのbind先は既定のloopbackを使ってください。

### 5. launchdでログイン時に自動起動する

[examples/com.suisan.hiro-maild.plist](examples/com.suisan.hiro-maild.plist)の`YOUR_USER`、バイナリのパス、ストアのパスを実際の**絶対パス**へ変更し、`~/Library/LaunchAgents/com.suisan.hiro-maild.plist`に配置します。plist内では`~`や環境変数を展開しません。例は送信確認のため`--forward-dry-run`を付けてあります。確認後にその1要素だけを削除して本運用にします。認証情報はplistに書かないでください。

```bash
mkdir -p "$HOME/.local/bin" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs/hiro-maild"
cp target/release/hiro-maild "$HOME/.local/bin/hiro-maild"
chmod 700 "$HOME/Library/Logs/hiro-maild"
cp examples/com.suisan.hiro-maild.plist "$HOME/Library/LaunchAgents/com.suisan.hiro-maild.plist"
# エディターでプレースホルダーを置換し、パスを確認
plutil -lint "$HOME/Library/LaunchAgents/com.suisan.hiro-maild.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.suisan.hiro-maild.plist"
launchctl kickstart "gui/$(id -u)/com.suisan.hiro-maild"
launchctl print "gui/$(id -u)/com.suisan.hiro-maild"
# 停止・設定変更前
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.suisan.hiro-maild.plist"
```

`--forward-dry-run`を外してplistを変更した後は、同じbootstrap/kickstartで再登録します。launchd用に配置したバイナリでもキーチェーンへアクセスできることを、対話的な初回実行で確認してください。

LaunchAgentはログイン中だけ動作します。Macがスリープ中は取り込みません。復帰後の周期で追いつきます。**Thunderbirdが起動して新着をローカル同期する必要があります**。macOSのログイン項目へThunderbirdを追加してください。hiro-maildはThunderbirdの起動・同期を操作しません。ログは件数と送信ID/状態が中心ですが、dry-runには件名・メールアドレスなどが出るため、ログもメールデータとして管理してください。ローテーションは別途必要です。

## 重複防止と障害時の操作

- Message-IDを優先し、ない場合はSHA-256で取込を重複排除します。Thunderbirdの`X-Mozilla-Status` / `Status2` / `Keys`はSHA判定から除外します。原本MIMEには残します。
- 転送対象・宛先と、メッセージごとの状態、試行回数、次回時刻、Gmail応答IDをSQLiteへ保存します。個々の試行結果も`forward_attempts`に残します。
- 送信前に`sending`を確定し、成功応答の後に`sent`を確定します。`sent`を再同期・再起動で再送しません。転送中のCLI/daemon重複起動は`forward.lock`で排除します。
- 接続確立失敗とHTTP 429は`retry`となり、60秒から最大1時間の指数バックオフで次回以降の実行時に再試行します。単発`forward`はその場で待たず終了します。
- HTTP 4xx（408/429以外）は`blocked`。認証・サイズ等を修正して明示的に再試行します。OAuth refresh失敗はメール送信前に止まり、daemonは次周期に再接続を試します。
- timeout、接続リセット、HTTP 408/5xx、成功応答が読めない場合は`unknown`に保留します。送信途中でプロセスが停止した`sending`も次回転送/再試行で`unknown`へ変更します。

```bash
./target/release/hiro-maild forward-status --limit 100
# retry / blockedを再度キューに入れるだけ（このコマンド自体は送信しない）
./target/release/hiro-maild forward-retry --id 123
# unknownはGmailを確認し、重複の可能性を承知した場合のみ
./target/release/hiro-maild forward-retry --id 123 --accept-duplicate-risk
./target/release/hiro-maild forward --limit 1
```

**exactly-once送信は保証できません。** Googleが受理した直後に接続やプロセスが切れると、ローカルには結果が残りません。Gmail APIにはこの処理で使える送信の冪等性キーがなく、同じMessage-IDを再利用しても重複排除は保証されません。外側のMessage-IDはメールと宛先から安定生成され、`forward-status`に表示されます。Gmailで`in:anywhere rfc822msgid:hiro-maild.…@hiro-maild.local`を検索してから判断してください。`gmail.send`だけを使用するため、アプリがGmailを検索して照合する機能はありません。

旧DBでMessage-IDがないメールは、元のraw hashが一致する再同期で移行します。移行前にローカルフラグや内容が変わって原本もない場合、同一メールと判断できず新規取込となる可能性があります。Message-ID自体が変わった場合、本文や改行が変わったMessage-IDなしメール、Thunderbirdがまだ完全同期していないメールも同一性/完全性を保証できません。初回はThunderbirdの同期完了を待ってください。

## その他のコマンドと検証

```bash
hiro-maild sync --store /absolute/path/to/store
hiro-maild list --limit 20
hiro-maild serve
# 任意の既存AI機能。転送/daemonからは呼ばれない
hiro-maild triage --dry-run --limit 3

cargo check --all-targets
cargo test --all-targets
```

テストは架空mboxとモック送信を使用し、実メール・Google認証情報を使いません。本文・添付バイト・HTML/CID・入れ子メール・原本、開始範囲、dry-run、再起動後の重複防止、バックオフ、結果不明、旧DB補完、同時起動を検証します。実アカウント検証の残りは[IMPLEMENTATION_STATUS.md](IMPLEMENTATION_STATUS.md)を参照してください。
