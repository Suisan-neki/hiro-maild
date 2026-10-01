# Macのブラウザ画面から、Thunderbird経由で手動転送

大学の独自OAuthアプリに対する管理者承認を待たずに使うため、受信は大学が公式に設定手順を案内するThunderbirdに任せます。hiro-maildはThunderbirdの同期済みmboxを読み、Gmail APIで認証した自分のGmailへ本文・添付・原本.emlを転送します。Microsoftのアプリ登録やGraphアクセスは使用しません。大学のサーバー側外部自動転送を設定せず、既読化・移動・削除も行いません。これは大学による本ツールや外部保管の承認を意味しません。

## 初回設定

1. [公式サイト](https://www.thunderbird.net/ja/)からThunderbirdをMacへインストールします。[大学の公式手順](https://www.media.hiroshima-u.ac.jp/services/hirodaimail/mail-app/)に沿って、大学アカウントを**IMAP / OAuth2**で設定します。大学ログイン・MFAは本人が行ってください。このインポーターにはmboxが必要なので、Exchangeやmaildirを選ばないでください。
2. Thunderbirdの「同期とディスク領域」でメッセージ全体を同期し、本文・添付がオフラインでも読めることを確認します。サーバーとの同期が終わるまで待ちます。読むだけで既読化したくない場合はThunderbirdの自動既読設定も確認してください。hiro-maild自体はThunderbirdや大学へ書き込みません。
3. 「アカウント設定 → サーバー設定 → メッセージの保存先」で大学のローカルフォルダーの絶対パスを確認します。通常は `~/Library/Thunderbird/Profiles/…/ImapMail/outlook.office365.com` です。自動検出できれば入力不要です。複数候補なら画面で選択します。指定フォルダー内の全mboxが対象なので、不要な送信済み・ゴミ箱等を除く場合は対象フォルダーだけの読み取り用コピーを指定します。
4. リポジトリでビルドして起動します。

```bash
cargo build --release --locked
mkdir -p "$HOME/Library/Application Support/hiro-maild"
chmod 700 "$HOME/Library/Application Support/hiro-maild"
./target/release/hiro-maild local-web
```

表示された `http://127.0.0.1:8082/` を**同じMac**のブラウザで開きます。保存先が未検出なら画面で設定します。既存CLIのデータフォルダーを使う場合は `--data-dir /absolute/path`、保存先を明示する場合は `local-web --store /absolute/path` を指定できます。データフォルダーを変更すると履歴も別になります。

5. 個人Gmailの宛先を入力し、開始位置を明示して保存します。「今から」は初回の全取り込みを終えた後の最大IDと現在日時を保存し、後から同期された過去メールも除外します。「日時指定」は元メールのDate以降の既存分も含めます。不明な日時は除外します。宛先・開始位置・保存先は初期化後に変更できません。
6. 「対象を確認（送信なし）」で件名・日時・添付名・MIMEの保存状況を確認します。この確認にはGmail認証が不要で、Gmailに通信・送信せず、転送キューも作成しません。ローカルの取り込みとSQLite保存だけを行います。
7. 既存のGoogle Cloudプロジェクトで、Gmail API、External / Testing、自分のテストユーザー、`gmail.send` / `openid` / `email` を設定します。OAuthクライアントは**Desktop app**を追加作成し、JSONをダウンロードします。GitHub Pages用Web applicationのJSONは使えません。JSONはリポジトリや同期共有フォルダーへ保存しないでください。
8. 画面の「初回のGmail認証設定」でJSONを選択し、「Gmailへ接続」→「Googleで認証する」を開きます。Googleの同意とMacのキーチェーン確認は本人が行います。指定のGmailと異なるアカウントは拒否します。JSONと認証URLはローカルプロセスのメモリだけで扱い、検証後の資格情報はMacのキーチェーンへ保存します。ログ・SQLite・リポジトリへ保存しません。2回目以降はJSONを選ばず接続します。Testingでは通常7日で再認証が必要です。

## 毎日の操作

Thunderbirdを起動して新着の同期を待ちます。hiro-maildの画面でGmailに接続し、送信なしで対象を確認して「同期して転送」を押します。1回に最大20通です。初回の実送信は少数で本文・添付を照合してください。この画面の「同期」はThunderbirdへの新着取得命令ではなく、ローカル保存済みデータの読み込みです。

画面を閉じても開始済みのローカル処理は続きますが、定期転送は行いません。プロセスを止めたときやMacのスリープ中は処理しません。SQLiteの転送履歴と排他ロックはCLI・daemonと共通です。元メールのMessage-IDまたはSHA-256、送信済み履歴により再取り込み・再起動の重複を防ぎます。

通信失敗はバックオフを保存して次回の操作で再試行します。送信結果が不明な場合は保留し、Gmailの `rfc822msgid:` 検索で到着を確認してから明示的に再試行を予約します。Googleが受理した直後に通信が切れると再送で重複する可能性があり、exactly-onceは保証しません。

## Macログイン時にローカル画面を起動する

`examples/com.suisan.hiro-maild-local-web.plist` の `YOUR_USER` とバイナリのパスを置換し、`~/Library/LaunchAgents/com.suisan.hiro-maild-local-web.plist` に保存します。plistにはパス・起動引数だけを書き、認証情報を書きません。ThunderbirdもmacOSのログイン項目へ追加し、起動後に同期できるようにします。

```bash
mkdir -p "$HOME/.local/bin" "$HOME/Library/LaunchAgents"
cp target/release/hiro-maild "$HOME/.local/bin/hiro-maild"
sed "s|/Users/YOUR_USER|$HOME|g" examples/com.suisan.hiro-maild-local-web.plist > "$HOME/Library/LaunchAgents/com.suisan.hiro-maild-local-web.plist"
plutil -lint "$HOME/Library/LaunchAgents/com.suisan.hiro-maild-local-web.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.suisan.hiro-maild-local-web.plist"
```

ブラウザは `http://127.0.0.1:8082/` をブックマークしてください。停止は `launchctl bootout` で同じplistを指定します。以前の `daemon --forward` を同時に自動起動している場合は、そのdaemonを停止してください。ローカル画面はloopbackにのみbindし、Host・Origin・CSRFを検査し、CORSやMCPは公開しません。LAN・Render・GitHub Pagesからこのローカル画面へ接続する構成ではありません。

## 実機確認の残り

このMacではThunderbird 157.0と大学IMAP/OAuth2アカウントを設定し、サーバー接続・本文のダウンロード開始・mbox形式を確認しました。自動既読は無効です。Google Desktop appの登録とJSON保存も完了しました。完全同期後の実メールの取り込み、Googleの送信権限への同意・キーチェーン・実メールのGmail表示・Mac再ログイン時のlaunchd起動は未確認です。ローカル画面のLaunchAgentは現在稼働しています。自作MicrosoftアプリでのWeb接続は実際に `AADSTS90094` で大学管理者承認待ちになったため、このローカル方式ではその接続を使用しません。
