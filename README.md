# hiro-maild

広島大学の広大メールを、Thunderbird のローカル同期を入口にして **read-only** で取り込み、添付を保存し、SQLite に蓄積してトリアージする Rust CLI です。

大学メール側へ直接書き込みません。メールの既読化・移動・削除・返信・送信は実装しません。

## なぜ Thunderbird を挟むか

広大メールは Microsoft 365 / Exchange Online で、IMAP4s (`outlook.office365.com:993`) + OAuth2 が公式に案内されています。一方、自作 OAuth アプリは大学テナントの同意ポリシーに左右されます。Thunderbird は広島大学が OAuth2 対応クライアントとして案内しているため、同期だけ Thunderbird に任せ、hiro-maild はローカルファイルのみ読む構成にしています。

## 現在の範囲

- Thunderbird の Outlook IMAP ローカルストアを検出
- mbox を read-only 解析
- `Message-ID`、なければ raw message の SHA-256 で重複排除
- 件名、送信者、日時、本文を SQLite 保存
- MIME 添付を安全なファイル名でローカル保存
- PDF / DOCX / PPTX / XLSX / TXT / CSV / ICS 等からトリアージ用テキスト抽出
- OpenAI Responses API + Structured Outputs で構造化トリアージ
- AI 呼び出し時は `store: false`
- Calendar への書き込み、返信、既読変更は未実装（意図的）

## CLI

```bash
# 状態確認
cargo run -- doctor

# Thunderbird から取り込み
cargo run -- sync

# 候補が複数ある場合
cargo run -- sync --store "/path/to/Thunderbird/Profiles/.../ImapMail/outlook.office365.com"

# 最近のメール
cargo run -- list --limit 20

# 未トリアージのみ
cargo run -- list --untriaged

# APIへ送る内容だけ確認（送信しない）
cargo run -- triage --dry-run --limit 3

# AIトリアージ
export OPENAI_API_KEY="..."
cargo run -- triage --limit 20
```

データディレクトリは OS ごとのアプリデータ領域を使います。明示したい場合:

```bash
export HIRO_MAILD_DATA_DIR="$HOME/hiro-maild-data"
```

## Thunderbird 側の前提

広島大学の公式設定どおり、Thunderbird に以下を設定します。

- IMAP server: `outlook.office365.com`
- port: `993`
- SSL/TLS
- OAuth2
- username: `IMCアカウント@hiroshima-u.ac.jp`

さらに「同期とディスク領域」でメッセージ本文をローカル同期しておく必要があります。hiro-maild は Thunderbird の `ImapMail` 配下にある mbox を読むだけです。

## データモデル

```text
messages
  1 ── N attachments
  1 ── 0..1 triage
```

トリアージは概ね以下です。

```json
{
  "importance": "high",
  "category": "academic",
  "summary": "再試験の日程案内",
  "requires_action": true,
  "action": "指定されたフォームを提出する",
  "deadline": "2026-09-12",
  "calendar": {
    "title": "病理学 再試験",
    "start": "2026-09-18T14:00:00+09:00",
    "end": null,
    "location": null
  },
  "confidence": 0.93
}
```

明記されていない締切・日時・場所は推測せず `null` にします。

## セキュリティ上の境界

- Thunderbird profile は読むだけ。hiro-maild から変更しない。
- 添付の送信者指定ファイル名はパスとして信用しない。
- AI 利用は明示的に `triage` を実行した場合のみ。
- API key を DB やリポジトリへ保存しない。
- 返信・削除・既読化などの mutation は MVP に入れない。
