// Only public client IDs, account identity, mail and delivery history belong here.
// OAuth tokens never enter this module.
const request = (r) =>
  new Promise((resolve, reject) => {
    r.onsuccess = () => resolve(r.result);
    r.onerror = () =>
      reject(
        new Error("ブラウザーへの保存に失敗しました。送信は続行しません。"),
      );
  });
export class Ledger {
  static async open(name = `hiro-maild-manual-v1:${globalThis.location?.pathname || "/"}`) {
    const r = indexedDB.open(name, 1);
    r.onupgradeneeded = () => {
      r.result.createObjectStore("meta", { keyPath: "key" });
      r.result.createObjectStore("messages", { keyPath: "key" });
      r.result.createObjectStore("aliases", { keyPath: "id" });
    };
    return new Ledger(await request(r));
  }
  constructor(db) {
    this.db = db;
  }
  async transaction(stores, mode, operation) {
    const tx = this.db.transaction(stores, mode);
    const complete = new Promise((resolve, reject) => {
      tx.oncomplete = resolve;
      tx.onabort = tx.onerror = () =>
        reject(
          new Error(
            "ブラウザーへの保存に失敗しました。空き容量と保存設定を確認してください。",
          ),
        );
    });
    try {
      const value = await operation(tx);
      await complete; // Intent must be committed before any Gmail POST.
      return value;
    } catch (e) {
      try {
        tx.abort();
      } catch {
        /* already aborted */
      }
      await complete.catch(() => {});
      throw e;
    }
  }
  get(key) {
    return this.transaction(
      ["meta"],
      "readonly",
      async (tx) => (await request(tx.objectStore("meta").get(key)))?.value,
    );
  }
  set(key, value) {
    return this.transaction(["meta"], "readwrite", (tx) =>
      request(tx.objectStore("meta").put({ key, value })),
    );
  }
  initialize(value, now = Date.now()) {
    return this.transaction(["meta"], "readwrite", async (tx) => {
      const store = tx.objectStore("meta");
      if (await request(store.get("start")))
        throw new Error("開始位置は設定済みです。");
      if (
        !Number.isFinite(Date.parse(value.since)) ||
        Date.parse(value.since) > now
      )
        throw new Error("開始日時は現在以前を指定してください。");
      if (!value.gmail || !value.microsoftId)
        throw new Error("両方のアカウントに接続してください。");
      await request(
        store.put({
          key: "start",
          value: { ...value, since: new Date(value.since).toISOString() },
        }),
      );
    });
  }
  rows() {
    return this.transaction(["messages"], "readonly", (tx) =>
      request(tx.objectStore("messages").getAll()),
    );
  }
  seen(id) {
    return this.transaction(["aliases"], "readonly", (tx) =>
      request(tx.objectStore("aliases").get(id)),
    );
  }
  enqueue(id, row) {
    return this.transaction(
      ["messages", "aliases"],
      "readwrite",
      async (tx) => {
        const messages = tx.objectStore("messages");
        if (!(await request(messages.get(row.key))))
          await request(messages.add(row));
        await request(tx.objectStore("aliases").put({ id, key: row.key }));
      },
    );
  }
  update(key, change) {
    return this.transaction(["messages"], "readwrite", async (tx) => {
      const store = tx.objectStore("messages");
      const row = await request(store.get(key));
      if (!row) throw new Error("転送履歴が見つかりません。");
      const updated = change(row);
      await request(store.put(updated));
      return updated;
    });
  }
  async recover() {
    return this.transaction(["messages"], "readwrite", async (tx) => {
      const store = tx.objectStore("messages");
      for (const row of await request(store.getAll())) {
        if (row.status === "sending")
          await request(
            store.put({
              ...row,
              status: "unknown",
              error: "画面終了などで送信結果を確認できませんでした。",
            }),
          );
      }
    });
  }
  retry(key, acceptRisk = false) {
    return this.update(key, (row) => {
      if (!["retry", "blocked", "unknown"].includes(row.status))
        throw new Error("このメールは再試行できません。");
      if (row.status === "unknown" && !acceptRisk)
        throw new Error("Gmailを確認し、重複リスクを了承する必要があります。");
      if (!row.raw)
        throw new Error("サイズ上限を超えたメールは手動で確認してください。");
      return { ...row, status: "pending", nextAt: 0, error: null };
    });
  }
}
export async function exclusive(
  operation,
  locks = globalThis.navigator?.locks,
) {
  if (!locks)
    throw new Error(
      "このブラウザーは安全な排他処理に未対応です。最新のSafari・Chrome・Firefoxを利用してください。",
    );
  return locks.request(
    "hiro-maild-manual-forward",
    { ifAvailable: true },
    (lock) => {
      if (!lock)
        throw new Error("別のタブで同期中です。終了してから操作してください。");
      return operation();
    },
  );
}
