# Web版の設定とRenderへの公開

MacやThunderbirdを起動せず、ブラウザーでGoogleと大学のMicrosoftアカウントを接続する構成です。Rustサービスが画面、OAuth callback、定期取り込み、Gmail送信を担当します。GitHubはソースとCI、Renderは常時実行と永続保存を担当します。[GitHub Pagesは静的ホスティング](https://docs.github.com/en/pages/getting-started-with-github-pages/what-is-github-pages)なので、この処理の実行先にはしません。

**1つのデプロイにつき1人用**です。`HIRO_MAILD_OWNER_GMAIL`に設定した、確認済みの個人Gmailでだけログインできます。別の人が利用する場合は、自分のRenderサービス・保存領域・OAuthアプリを用意してください。複数人のアカウントを1つのサービスに登録する機能はありません。

## 1. GitHubからRenderを作成する

1. [Render Dashboard](https://dashboard.render.com/)でGitHubを連携し、New → Blueprintからこのリポジトリを選びます。レビュー中は`codex/gmail-forwarding`ブランチ、マージ後は`main`を選択してください。
2. ルートの`render.yaml`を使います。Rust 1.95.0、ビルド`cargo build --release --locked`、起動`./target/release/hiro-maild web`、ヘルスチェック`/healthz`が設定済みです。
3. `HIRO_MAILD_OWNER_GMAIL`に自分の個人Gmailを入力します。まずOAuth未設定で公開URLを確定し、次の手順でアプリを登録して、サービスのEnvironmentに4項目を追加します。その間、画面にはログイン設定待ちと表示されます。
4. 有料のWebサービス（`0.5c-512mb`）と1 GBの永続ディスクを作成する構成です。作成画面で料金を確認してからデプロイしてください。無料の一時ファイル領域では転送履歴を保てません。[永続ディスクの公式説明](https://render.com/docs/disks)
5. Renderが発行した実際の`https://…onrender.com`を控えます。`RENDER_EXTERNAL_URL`と`PORT`はRenderから取得するため、URLの推測は不要です。カスタムドメインを使う場合だけ`HIRO_MAILD_PUBLIC_URL=https://自分のドメイン`を追加します。

ブランチを直接公開する場合のリンク：

[Deploy to Render（レビュー用ブランチ）](https://render.com/deploy?repo=https%3A%2F%2Fgithub.com%2FSuisan-neki%2Fhiro-maild%2Ftree%2Fcodex%2Fgmail-forwarding)

このリンクは有料リソースを自動作成済みという意味ではありません。Renderで内容の確認と作成が必要です。[ブランチを指定する公式手順](https://render.com/docs/deploy-to-render#specifying-a-branch)

`autoDeployTrigger: checksPass`により、連携ブランチのGitHub CI成功後にRenderで再ビルドします。更新先ブランチを変えるときはRenderのBlueprint/サービス設定も合わせてください。公開されたコードが認証情報を扱うため、他人が利用する場合は自分のforkに連携する運用を推奨します。

## 2. GoogleのWeb OAuthアプリ

1. [Google Cloud Console](https://console.cloud.google.com/)でプロジェクトを作成し、Gmail APIを有効にします。
2. Google Auth PlatformのAudienceをExternalにします。Testingなら自分のGmailをテストユーザーに追加します。
3. Data Accessに`openid`、`email`、`https://www.googleapis.com/auth/gmail.send`を設定します。Gmailの読取・変更・削除権限は不要です。
4. OAuth clientを**Web application**で作成します。Desktop app用クライアントとは別です。Authorized redirect URIsに、実際の公開URLを使って以下を完全一致で登録します。

   `https://実際の公開ホスト/auth/google/callback`

5. client IDを`GOOGLE_CLIENT_ID`、secretを`GOOGLE_CLIENT_SECRET`としてRenderのEnvironmentに設定します。secret JSONをGitHubへアップロードしないでください。

[Google Web OAuthの公式説明](https://developers.google.com/identity/protocols/oauth2/web-server)に従い、Authorization Code + PKCEとサーバー側のsecretで認証します。Googleの確認済みemailを所有者設定と照合し、アクセストークンをブラウザーへ渡しません。

External/Testingのrefresh tokenは、このメール送信権限を含む場合[通常7日で期限切れ](https://developers.google.com/identity/protocols/oauth2#expiration)になります。Testingのままなら画面から再接続が必要です。Productionへの変更や他人向け公開は、Google側のアプリ審査・利用条件も確認してください。長期利用できるかは実アカウントで未検証です。

## 3. MicrosoftのWeb OAuthアプリ

1. [Microsoft Entra管理センター](https://entra.microsoft.com/)のApp registrationsでアプリを作成します。自分が管理できるテナントで登録する場合は「任意の組織ディレクトリのアカウント」を選びます。広大テナントだけのアプリを大学管理者が登録する場合は、そのテナントのIDを`MICROSOFT_TENANT_ID`に設定します。
2. Authentication → Webに、実際の公開URLを完全一致で登録します。

   `https://実際の公開ホスト/auth/microsoft/callback`

3. Microsoft Graphの**Delegated permissions**を`User.Read`、`Mail.Read`にします。offline accessを要求します。Application permissionsや`Mail.ReadWrite`、`Mail.Send`は使いません。
4. Certificates & secretsでclient secretを作成します。Application (client) IDを`MICROSOFT_CLIENT_ID`、secretの**Value**を`MICROSOFT_CLIENT_SECRET`としてRenderに設定します。Secret IDではありません。secretの期限切れ時には更新が必要です。
5. 通常は`MICROSOFT_TENANT_ID=organizations`のまま、画面で広島大学アカウントにログインします。確認したmailまたはUPNが`@hiroshima-u.ac.jp`であることと、GraphのアカウントIDを検証します。最初に接続した大学アカウントに固定し、別アカウントへの切り替えは拒否します。

[Microsoft認証の公式説明](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-auth-code-flow)。**大学側がアプリへの同意を制限している場合は管理者の承認が必要です。アプリ登録そのものができない場合もあります。** コードだけでこの制限を解除することはできません。実アカウントでの同意・読取は未検証です。

## 4. 画面から接続して開始する

1. 公開URLを開き「Googleでログイン」。設定した所有者Gmailで同意します。ログインだけでは送信しません。
2. 「Microsoftで接続」。大学アカウントで読取に同意します。
3. 「今から受信するメール」を選び、開始位置を保存します。過去分も必要なら日時を選びます。入力日時はブラウザーのタイムゾーンとして扱い、UTCへ変換します。開始位置・宛先は一度保存すると変更できません。
4. 「対象を確認（送信なし）」を押します。大学のInboxを読んでローカルに保存し、転送候補の件名・送信者・元日時・添付名を表示します。この操作はGmail送信を呼び出しません。転送を有効にした状態では別の定期処理が送信を続けるため、落ち着いて確認するには先に停止してください。
5. 対象を確認できたら「転送を開始」。既定は60秒ごと、1サイクル最大20通を送信します。転送を止めるには「転送を停止」。処理中の1通は完了する場合があります。
6. 初めての実運用では受信した1通の本文、添付、`original.eml`をGmailで確認します。同じGmailから自分宛の送信がInboxに表示されるか、迷惑メール扱いにならないかも確認してください。

ブラウザーを閉じる・ログアウトする操作では自動転送を止めません。画面のログインは24時間で切れ、再ログインが必要です。停止していても受信の定期読み取りは続きます。

## 読取範囲とMIME

Graphで**Inboxのみ**を読みます。大学側のルールで別フォルダーへ直接振り分けられるメール、取り込み前にInboxから移動・削除されたメール、迷惑メールフォルダーのメールは対象外です。大学のメーラーで大学側の受信も確認してください。

開始位置は[Graphの`receivedDateTime`](https://learn.microsoft.com/en-us/graph/api/message-delta?view=graph-rest-1.0)で判定します。送信者の古い/不正なDateとは別です。ImmutableId、nextLink/deltaLink、Graph IDの取込履歴を保存し、元Message-ID（なければSHA-256）でも重複排除します。delta状態が期限切れ（410）になれば同じ開始範囲から再取得し、既存履歴で重複を防ぎます。各ページの保存が終わる前にカーソルを進めません。1サイクル5ページまでで、残りは次回へ続けます。

[Graphの`/$value`](https://learn.microsoft.com/en-us/graph/api/message-get?view=graph-rest-1.0)が返した全MIMEをSQLiteへバイト単位で保存します。サーバーからの取得上限は32 MiB、転送の内部上限は原本18 MiB・組立後34 MiBです。取得不能・不正なMIMEで同期が止まる場合は接続と対象メールを確認してください。転送上限超過は保留し、添付を黙って省略しません。

元のContent-*ヘッダーと本文のMIME構造を包んで転送し、本文・HTML・添付・Content-ID・入れ子メールを保持します。さらに全原本を`original.eml`で添付します。外側のFrom/Toは個人Gmail、Dateは転送時刻、件名は`[広大メール]`付きです。元送信者・日時は本文の転送情報と原本ヘッダーに残ります。元DKIMや署名は新しい転送メールを認証しません。暗号化を解除する機能もありません。

## 失敗と重複

送信前にSQLiteへ送信意図を確定し、成功時にGmail message IDを保存します。既知の接続失敗と429は60秒〜1時間のバックオフで再試行します。認証失効・不正なMIME等は保留します。接続の再設定後に履歴から再試行を予約できます。

送信途中のtimeout/reset、5xx、成功応答が不正、送信後の保存失敗やプロセス中断は**結果不明**です。次の転送実行で中断状態を結果不明へ回収し、自動再送しません。Gmailの検索欄に履歴の`rfc822msgid:…`を貼り付け、受信済みか確認してください。「確認して再試行」は重複リスクを了承したときだけ使います。同じMessage-IDでもGmail側が必ず重複排除する保証はなく、exactly-onceは保証できません。Gmailの読取権限を持たないため自動照合はしません。

## 保存と運用

- 永続ディスク`/var/data/hiro-maild`にDB（原本・送信履歴・同期位置）と添付を置きます。メール本文・添付はこのサーバーにも保存されます。認証トークンだけがアプリ側で暗号化され、メール本文自体はアプリ暗号化されません。Renderの保存領域の保護と管理者権限に依存します。
- refresh tokenはChaCha20-Poly1305で暗号化し、provider・emailへ紐付けます。`HIRO_MAILD_TOKEN_KEY`はRenderが初回にランダム生成します。**再デプロイ時にも保持し、DBとは別に安全にバックアップしてください。** 失うと再ログインが必要です。既存DBを別の所有者へ使い回さないでください。
- client secretと暗号鍵はRenderのEnvironmentだけに設定します。GitHub・ブラウザー保存領域・ログには入れません。アクセストークンはメモリだけ、セッションcookieはHttpOnly/Secure/SameSite、変更操作はOriginとCSRF tokenを確認します。OAuth callback queryには認証コードが含まれるため、URL全体のアクセスログを別途追加しないでください。
- 永続ディスク付きの単一インスタンスで実行します。水平スケール、別サービスとの同じDB共有、複数送信workerの追加は対象外です。プロセスロックで1サイクル全体を排他します。再デプロイによる停止中は受信を読めず、次回の同期で追いつきます。
- DBと添付の削除・古いバックアップへの巻き戻しは、送信済み履歴を失い重複送信につながります。復元時はまず停止して履歴とGmailを照合してください。保存済みメールを自動削除する機能はなく、ディスク使用量は増えます。容量はRenderで監視・拡張してください。
- [大学の公式注意事項](https://www.media.hiroshima-u.ac.jp/services/hirodaimail/notice/)はサーバー側外部自動転送を設定しないよう案内しています。このアプリは外部転送設定を使いません。この方法や外部サーバーへのメール保管が大学から承認されたという意味でもありません。

## ローカルで画面を確認する（認証・送信なし）

```bash
cargo build --locked
export HIRO_MAILD_DATA_DIR="$(mktemp -d)"
export HIRO_MAILD_PUBLIC_URL=http://127.0.0.1:8080
export HIRO_MAILD_OWNER_GMAIL=you@gmail.com
export HIRO_MAILD_TOKEN_KEY="$(openssl rand -hex 32)"
./target/debug/hiro-maild web
```

`http://127.0.0.1:8080`を開くと、OAuth未設定の画面を確認できます。`.env.example`は設定名の参考です。プログラムは`.env`を自動読込しません。実アカウント用client secretをコマンド行の引数や共有ログへ書かないでください。

Web版は独自の定期workerを持ち、ローカルMCPを公開しません。従来のThunderbird・Mac Keychain・launchd方式もCLIに残っていますが、Web版とは別のデータディレクトリを使ってください。Web版ではMacの自動起動設定は不要です。
