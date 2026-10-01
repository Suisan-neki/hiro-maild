import { firstPage, graphLink } from "./api.js";
export class Engine {
  constructor(ledger, api, core, identity, now = () => Date.now()) {
    Object.assign(this, { ledger, api, core, identity, now });
    this.stopped = false;
  }
  stop() {
    this.stopped = true;
  }
  async settings() {
    const s = await this.ledger.get("start");
    if (!s) throw new Error("転送開始位置を先に設定してください。");
    const identity = this.identity();
    if (s.gmail !== identity.gmail || s.microsoftId !== identity.microsoftId)
      throw new Error(
        "開始設定と異なるアカウントです。元のアカウントに接続してください。",
      );
    return s;
  }
  async sync(progress = () => {}) {
    const s = await this.settings();
    let cursor = (await this.ledger.get("cursor")) || firstPage(s.since);
    let imported = 0,
      expired = false,
      finished = false;
    // Large backlogs are processed in bounded batches. The next run resumes the page.
    for (let p = 0; p < 10 && !this.stopped; p++) {
      let page;
      try {
        page = await this.api.page(cursor);
      } catch (e) {
        if (e.code !== "EXPIRED" || expired) throw e;
        expired = true;
        cursor = firstPage(s.since);
        await this.ledger.set("cursor", cursor);
        p--;
        continue;
      }
      for (const item of page.value) {
        if (this.stopped) return { imported, finished: false };
        if (item["@removed"]) continue;
        if (typeof item.id !== "string" || !item.id)
          throw new Error("大学メールのIDが不正です。");
        if (
          !Number.isFinite(Date.parse(item.receivedDateTime)) ||
          Date.parse(item.receivedDateTime) < Date.parse(s.since)
        )
          continue;
        if (await this.ledger.seen(item.id)) continue;
        let raw;
        try {
          raw = await this.api.mime(item.id);
        } catch (e) {
          if (e.code === "MISSING") continue;
          if (e.code !== "SIZE") throw e;
          const mid = (item.internetMessageId || "")
            .trim()
            .replace(/^<|>$/g, "");
          const key = mid ? `mid:${mid}` : `graph:${item.id}`;
          await this.ledger.enqueue(item.id, {
            key,
            subject: item.subject || "(件名なし)",
            sender_address: item.from?.emailAddress?.address || "",
            receivedAt: item.receivedDateTime,
            status: "blocked",
            error: e.message,
            attempts: 0,
            nextAt: 0,
            forwardId: this.core.forwardId(key, s.gmail),
          });
          continue;
        }
        let metadata;
        try {
          metadata = this.core.inspect(raw);
        } catch {
          const mid = (item.internetMessageId || "")
            .trim()
            .replace(/^<|>$/g, "");
          const key = mid ? `mid:${mid}` : `graph:${item.id}`;
          await this.ledger.enqueue(item.id, {
            key,
            raw,
            subject: item.subject || "(件名なし)",
            sender_address: item.from?.emailAddress?.address || "",
            receivedAt: item.receivedDateTime,
            status: "blocked",
            error: "原本MIMEが不正です。大学メールで手動確認してください。",
            attempts: 0,
            nextAt: 0,
            forwardId: this.core.forwardId(key, s.gmail),
          });
          continue;
        }
        let error = null;
        try {
          this.core.compose(raw, s.gmail, this.now());
        } catch {
          error = "MIMEが不正、または転送サイズの上限を超えています。";
        }
        await this.ledger.enqueue(item.id, {
          ...metadata,
          key: metadata.stable_key,
          raw,
          receivedAt: item.receivedDateTime,
          status: error ? "blocked" : "pending",
          error,
          attempts: 0,
          nextAt: 0,
          forwardId: this.core.forwardId(metadata.stable_key, s.gmail),
        });
        imported++;
        progress(`新しいメール ${imported} 件を保存しました。`);
      }
      cursor = graphLink(page["@odata.nextLink"] || page["@odata.deltaLink"]);
      await this.ledger.set("cursor", cursor); // Only after all original MIME/identities are durable.
      if (!page["@odata.nextLink"]) {
        finished = true;
        await this.ledger.set("lastSync", new Date(this.now()).toISOString());
        break;
      }
    }
    return { imported, finished };
  }
  async forward(progress = () => {}, maximum = 50) {
    const s = await this.settings();
    let sent = 0;
    const rows = (await this.ledger.rows()).sort(
      (a, b) => Date.parse(a.receivedAt) - Date.parse(b.receivedAt),
    );
    for (const row of rows) {
      if (this.stopped || sent >= maximum) break;
      if (!["pending", "retry"].includes(row.status) || row.nextAt > this.now())
        continue;
      let mime;
      try {
        mime = this.core.compose(row.raw, s.gmail, this.now());
      } catch {
        await this.ledger.update(row.key, (r) => ({
          ...r,
          status: "blocked",
          error: "MIMEが不正、または転送サイズの上限を超えています。",
        }));
        continue;
      }
      // Token/identity validation is read-only and happens before a durable submission claim.
      this.api.auth?.googleToken();
      await this.ledger.update(row.key, (r) => {
        if (!["pending", "retry"].includes(r.status))
          throw new Error("転送状態が変わりました。");
        return {
          ...r,
          status: "sending",
          attempts: r.attempts + 1,
          error: null,
        };
      });
      let outcome;
      try {
        outcome = await this.api.send(mime);
      } catch {
        outcome = {
          status: "unknown",
          error: "送信結果を確認できませんでした。Gmailを確認してください。",
        };
      }
      await this.ledger.update(row.key, (r) => ({
        ...r,
        ...outcome,
        nextAt:
          outcome.status === "retry"
            ? this.now() +
              Math.min(3600000, 60000 * 2 ** Math.min(6, r.attempts - 1))
            : 0,
        sentAt:
          outcome.status === "sent" ? new Date(this.now()).toISOString() : null,
        // Retain the small permanent ledger, release MIME only after confirmed success.
        raw: outcome.status === "sent" ? undefined : r.raw,
      }));
      if (outcome.status === "sent") {
        sent++;
        progress(`${sent} 件をGmailへ転送しました。`);
      }
      if (["blocked", "unknown", "retry"].includes(outcome.status)) break;
    }
    return sent;
  }
  async run({ dryRun = false, progress } = {}) {
    await this.ledger.recover();
    const result = await this.sync(progress);
    if (!dryRun && !this.stopped) result.sent = await this.forward(progress);
    return result;
  }
}
