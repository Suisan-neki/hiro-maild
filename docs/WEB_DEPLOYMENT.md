# 無料のブラウザー版：GitHub Pagesで手動同期

普段使うブラウザーで1日2回など「同期して転送」を押す構成です。**有料サーバー・Render・永続ディスク・Macの常時起動は不要**です。ブラウザーからMicrosoft Graphを読み、Gmail APIへ送信します。RustのMIME処理をWebAssemblyとして画面に同梱しています。画面を閉じると処理は止まります。GitHub Actionsはビルド・テスト・静的サイト公開だけを行い、メール処理や定期送信はしません。

公開先：`https://suisan-neki.github.io/hiro-maild/`。公開成功はGitHubの「browser-pages」workflowとSettings → Pagesで確認できます。レビュー用ブランチから公開済みです。サイトには秘密情報・メール・アカウント固有の設定を含めません。

## 1. GitHub Pagesを公開

この公開リポジトリでは[GitHub FreeでPagesを利用できます](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages)。Settings → Pages → Build and deployment → Sourceを**GitHub Actions**にします。

`.github/workflows/pages.yml`は`main`またはレビュー用の`codex/gmail-forwarding`へのpushで、Rust MIMEのテスト、WASMビルド、ブラウザーテストを通してから公開します。PRイベントではテストだけです。レビュー用ブランチから公開する場合はSettings → Environments → github-pagesのDeployment branchesに`codex/gmail-forwarding`を追加します（このリポジトリでは設定済み）。プラン変更やカード登録は不要です。forkで使う場合は自分のPages URLを以下の登録に使います。

## 2. Googleを初回登録

1. [Google Cloud Console](https://console.cloud.google.com/)でプロジェクトを作り、**Gmail API**を有効にします。
2. Google Auth Platform → AudienceをExternalにします。Testingの場合は、自分の個人Gmailをテストユーザーに追加します。
3. Data Accessに`openid`、`email`、`https://www.googleapis.com/auth/gmail.send`を設定します。Gmailの読取・変更・削除権限は不要です。
4. OAuth clientを**Web application**として作ります。Authorized JavaScript originsには **`https://suisan-neki.github.io`** を登録します。ここには`/hiro-maild/`などのパスを含めません。ポップアップのtoken modelなので、Google callbackやclient secretの入力は不要です。
5. 公開client ID（`…apps.googleusercontent.com`）だけをコピーします。画面の「初回のログイン設定」に入力します。ダウンロードしたsecret JSONをアップロードしないでください。

[Google公式のtoken model](https://developers.google.com/identity/oauth2/web/guides/use-token-model)を使用します。短期アクセストークンをユーザー操作で取得し、メモリだけで扱います。画面の再読込・終了後は再ログインします。期限切れ時は再接続します。refresh tokenをサーバー・GitHub・ブラウザーの永続領域に保存しません。GoogleのTesting/審査条件はその時点の管理画面を確認してください。他の人へ配布する場合も、自分のOAuthアプリで利用するか適切な同意・審査を行う必要があります。

## 3. 大学のMicrosoftアカウントを初回登録

1. [Microsoft Entra管理センター](https://entra.microsoft.com/) → App registrationsで自分が管理できるアプリを登録します。別テナントで登録する場合は「任意の組織ディレクトリのアカウント」を選択します。個人Microsoftアカウントはこの画面の対象外です。
2. Authentication → Add a platform → **Single-page application (SPA)**を選び、redirect URIを **`https://suisan-neki.github.io/hiro-maild/redirect.html`** として登録します。Webプラットフォームやclient secretは使いません。Implicit grantのaccess/ID tokenチェックは不要です。
3. API permissions → Microsoft Graph → **Delegated permissions**で`User.Read`、`Mail.Read`を追加します。`Mail.ReadWrite`、`Mail.Send`やapplication permissionsは不要です。
4. OverviewのApplication (client) IDを、画面のMicrosoft client IDへ入力します。
5. 画面から広島大学のMicrosoftアカウントでログインし、要求された読取権限へ同意します。

**大学テナントがアプリ連携やユーザー同意を禁止している場合は、大学管理者の承認が必要です。** アプリ登録権限がなく登録できない場合も管理者への相談が必要です。この実装ではその制限を回避できません。実アカウントでの同意可否は未確認です。

Microsoftの公式MSAL Browser 5を同梱し、Authorization Code + PKCEと専用[redirect bridge](https://learn.microsoft.com/en-us/entra/msal/javascript/browser/login-user#redirecturi-considerations)を使います。アクセストークン・MSALのrefresh token等は[memoryStorage](https://learn.microsoft.com/en-us/entra/msal/javascript/browser/caching)で扱います。MSALがログイン中だけ使うstate/PKCE等の一時データはsessionStorageに置きます。メール本文・認証エラー応答・トークンをログへ出力しません。

## 4. 普段の使い方（Macでも同じ）

1. 普段使うSafari・Chrome・Firefoxの通常ウィンドウで公開URLを開きます。プライベートモードやブラウザーデータの自動削除は避けてください。
2. 初回だけ上記の公開client IDを入力し「設定を保存」。以後IDは同じブラウザーに保存されます。
3. 「Gmailにログイン」と「大学のMicrosoftにログイン」。Googleは確認済みの個人`@gmail.com`、Microsoftは`@hiroshima-u.ac.jp`のアカウントを確認します。
4. 初回だけ「今から受信するメール」または過去の日時を明示して「開始位置を保存」。**今から**を選べば過去分を大量送信しません。開始日時・Gmail・大学アカウントIDは固定され、後のログイン先が違うと同期を拒否します。
5. 「対象を確認（送信なし）」でメールを取得・保存し、件名・送信者・対象件数・保留を確認します。GraphのGETだけを使い、Gmailへ送信しません。初回はこの確認が完了するまで送信ボタンを使えません。
6. 「同期して転送」で新着分と再試行待ちを処理します。画面を開いたまま、停止中の表示になるまで待ってください。
7. 次からは画面を開いて両方に接続し、「同期して転送」。受信してから次にボタンを押すまでGmailには届きません。タイマーによる定期実行はありません。

1回に最大10ページ（各ページ最大50件を希望）と50通を処理します。大量の過去分を選んだ場合は対象確認を繰り返して初回同期を完了し、その後に転送を繰り返してください。「終了を待って停止」は実行中の通信を完了してから止めます。画面終了・Macスリープ・ネットワーク切断では途中で停止する場合があります。

## 対象と保持する内容

大学側の**Inbox（受信トレイ）**のみをGraphで読みます。サブフォルダー・アーカイブ・送信済みは対象外です。受信日時`receivedDateTime`で開始位置と比較し、元送信者のDateには依存しません。受信日時が不明・不正なものは対象外です。同期前にInboxから移動・削除されたものは取得できません。大学側を既読化・移動・削除・返信するAPI呼出しはありません。

[GraphのMIME取得](https://learn.microsoft.com/en-us/graph/api/message-get?view=graph-rest-1.0)で全原本を取得します。本文・HTML・添付・CID・入れ子MIMEをバイト単位でコピーし、全原本も`original.eml`に追加添付します。外側のFrom/Toは個人Gmail、Dateは転送時刻、件名には`[広大メール]`を付けます。元送信者・元日時・元件名・元Message-IDは本文冒頭と原本ヘッダーに残ります。原本のDKIM/署名が新しいメールを認証するわけではありません。暗号化解除は行いません。Gmailの表示・添付検査・迷惑メール判定は実アカウントで未検証です。

原本18 MiB・組立後34 MiBの内部上限があります。原本コピーによる増量があるため、大学で受信できる全サイズを必ず転送できるわけではありません。上限超過は保留し、添付を黙って削りません。原本を取得できなかった上限超過メールは大学のWebメール等で手動確認してください。

## 履歴、失敗と再試行

履歴・大学アカウントID・宛先・同期位置・開始日時・公開client IDはIndexedDBに保存します。原本は転送待ち・保留中だけ同じ場所に保持し、Gmailの成功が確定したら原本を解放して小さな送信済み履歴を残します。取得直後に原本と重複キー（Message-ID、なければSHA-256）を確定してからページ位置を進めます。再同期・Graph位置期限切れの再取得で送信済み履歴を上書きしません。複数タブの処理はWeb Locksで排他します。

GmailへのPOST前に`送信中`を確定します。HTTP 429は60秒〜1時間のバックオフ後、次の手動同期で再試行します。認証や形式による拒否は保留し、接続を直して履歴から再試行を予約します。

**ネットワークエラー、timeout、408/5xx、不正な成功応答、送信後の保存失敗、送信途中の画面終了は結果不明です。** ブラウザーのfetchでは送信前後を確実に判別できないため、通信失敗を安易に自動再送しません。次の起動/同期で中断状態を結果不明へ回収します。Gmailの検索欄に画面の`in:anywhere rfc822msgid:…`を貼り、届いていないか確認してください。「Gmail確認後に再試行を予約」は重複リスクの確認後だけ実行できます。同じMessage-IDでもGmailが必ず重複排除する保証はなく、exactly-onceは保証しません。Gmailの読取権限を持たず自動照合はしません。

サイトデータ削除、別のブラウザー・端末・別のorigin、履歴の巻き戻しでは重複防止を引き継げません。**履歴を失った場合は同じ過去開始日時を再指定せず、Gmailを照合した上で「今から」を設定するなど、再送範囲を明示してください。** 自動バックアップ・端末間共有はありません。保存容量不足ではPOST前に停止します。ブラウザーがデータを退避・削除する可能性もあるため、永続保存の要求が許可されても消失を完全保証できません。

## データの扱いと公式案内

メールは大学・手元のブラウザー・自分のGmailの間で扱います。GitHub Pagesには静的コードだけを置き、GitHubへメールやトークンをアップロードしません。大学・GoogleへのHTTPS通信にはOAuth bearer tokenを使います。公開コードの更新はこの権限を持つ画面を変更できるため、信頼するリポジトリ/公開先を使ってください。共有端末ではメール履歴も共有されます。

[大学の公式注意事項](https://www.media.hiroshima-u.ac.jp/services/hirodaimail/notice/)はMicrosoft 365サーバー側の外部自動転送設定をしないよう案内しています。このアプリはサーバー側転送設定を使いません。この方式やGmailでの外部保管が大学から承認済みという意味ではありません。大学の利用ルールとメール内容に従って利用してください。

## ローカルで開発・検証

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack --version 0.15.0 --locked
cd browser
npm ci
npm run build
npm test
npm run dev
```

開発画面は`http://127.0.0.1:8081/`。Google originsにはこのorigin、Microsoft SPA redirectには`http://127.0.0.1:8081/redirect.html`を別途登録します。本番との履歴共有はありません。認証しない状態でも設定画面を確認できます。本番CSPのまま開発時のHMR接続は許可しないため、変更後は画面を再読込してください。

```bash
cargo check --all-targets --locked
cargo test --all-targets --locked
cargo test -p hiro-mail-core --locked
```

テストは架空メール・偽API・偽トークンのみです。実メールを送信しません。以前のRustサーバー版は[別構成の参考](SERVER_DEPLOYMENT.md)として残しますが、無料ブラウザー版の設定には使いません。CLI/Thunderbird/launchdも別の保存領域を持つ従来方式です。
