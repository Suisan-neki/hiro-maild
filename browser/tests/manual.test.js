import "fake-indexeddb/auto";
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFile } from "node:fs/promises";
import init, {
  inspect_mime,
  compose_mime,
  forward_id,
} from "../pkg/hiro_mail_core.js";
import { Ledger, exclusive } from "../src/ledger.js";
import { Engine } from "../src/engine.js";
import {
  MailApi,
  ApiError,
  firstPage,
  graphLink,
  base64url,
} from "../src/api.js";
const now = Date.parse("2026-10-01T12:00:00Z");
const identity = () => ({
  gmail: "student@gmail.com",
  microsoftId: "university-id",
});
const core = {
  inspect: (raw) => JSON.parse(inspect_mime(raw)),
  compose: (raw, gmail, ms) =>
    compose_mime(
      raw,
      gmail,
      "0123456789abcdef0123456789abcdef",
      BigInt(Math.floor(ms / 1000)),
    ),
  forwardId: forward_id,
};
const raw = (id = "test@example.com", body = "hello") =>
  new TextEncoder().encode(
    `From: Student Office <office@example.com>\r\nTo: student@hiroshima-u.ac.jp\r\nDate: Thu, 1 Jan 2020 09:00:00 +0900\r\nMessage-ID: <${id}>\r\nSubject: university notice\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n${body}`,
  );
const delta =
  "https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta?$deltatoken=done";
const page = (items) => ({ value: items, "@odata.deltaLink": delta });
const item = (id = "graph-1", received = "2026-10-01T13:00:00Z") => ({
  id,
  receivedDateTime: received,
});
class FakeApi {
  constructor(
    items = [item()],
    outcomes = [{ status: "sent", gmailId: "fake-sent" }],
  ) {
    this.items = items;
    this.outcomes = outcomes;
    this.sent = [];
    this.downloaded = [];
  }
  async page() {
    return page(this.items);
  }
  async mime(id) {
    this.downloaded.push(id);
    return raw();
  }
  async send(mime) {
    this.sent.push(mime);
    return this.outcomes.shift() || { status: "sent", gmailId: "fake-next" };
  }
}
async function setup(api = new FakeApi(), since = new Date(now).toISOString()) {
  const ledger = await Ledger.open(`test-${crypto.randomUUID()}`);
  await ledger.initialize({ ...identity(), since }, now);
  return {
    ledger,
    api,
    engine: new Engine(ledger, api, core, identity, () => now),
  };
}
beforeAll(async () => {
  await init({
    module_or_path: await readFile(
      new URL("../pkg/hiro_mail_core_bg.wasm", import.meta.url),
    ),
  });
});
describe("shared Rust MIME in WebAssembly", () => {
  it("keeps HTML, CID, binary attachments, nested message and the complete original bytes", () => {
    const original = new TextEncoder().encode(
      'From: Office <office@example.com>\r\nMessage-ID: <rich@example.com>\r\nSubject: =?UTF-8?B?5bqD5aSn44Oh44O844Or?=\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n--outer\r\nContent-Type: multipart/related; boundary=inner\r\n\r\n--inner\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p><img src="cid:pic">本文</p>\r\n--inner\r\nContent-Type: image/png\r\nContent-ID: <pic>\r\nContent-Disposition: inline; filename=pic.png\r\nContent-Transfer-Encoding: base64\r\n\r\nAAECA/8=\r\n--inner--\r\n--outer\r\nContent-Type: message/rfc822\r\n\r\nFrom: nested@example.com\r\nSubject: nested\r\n\r\nnested body\r\n--outer--\r\n',
    );
    const composed = core.compose(original, identity().gmail, now);
    const text = new TextDecoder().decode(composed);
    expect(text).toContain(
      new TextDecoder()
        .decode(original)
        .split("\r\n\r\n")
        .slice(1)
        .join("\r\n\r\n"),
    );
    expect(text).toContain("Content-ID: <pic>");
    expect(text).toContain("AAECA/8=");
    const encoded = text
      .split('filename="original.eml"')[1]
      .split("\r\n\r\n")[1]
      .split("\r\n--hiro-maild-")[0]
      .replaceAll("\r\n", "");
    expect(new Uint8Array(Buffer.from(encoded, "base64"))).toEqual(original);
    expect(text).toContain("From: student@gmail.com\r\nTo: student@gmail.com");
    expect(core.inspect(original).subject).toBe("広大メール");
  });
  it("rejects oversize MIME and header injection, while no-ID local flags do not change identity", () => {
    expect(() =>
      core.compose(raw(), "bad@gmail.com\r\nBcc: other@example.com", now),
    ).toThrow();
    expect(() =>
      core.compose(new Uint8Array(18 * 1024 * 1024 + 1), identity().gmail, now),
    ).toThrow();
    const a = new TextEncoder().encode(
      "X-Mozilla-Status: 0000\r\nSubject: a\r\n\r\nbody",
    );
    const b = new TextEncoder().encode(
      "X-Mozilla-Status: 0001\r\nSubject: a\r\n\r\nbody",
    );
    expect(core.inspect(a).stable_key).toBe(core.inspect(b).stable_key);
  });
});
describe("manual synchronization and persistent delivery history", () => {
  it("requires an explicit start, excludes backlog by RECEIVED time and dry-run never submits", async () => {
    const ledger = await Ledger.open(`uninitialized-${crypto.randomUUID()}`);
    const api = new FakeApi([
      item("old", "2020-01-01T00:00:00Z"),
      item("new"),
      item("unknown", "broken"),
    ]);
    const engine = new Engine(ledger, api, core, identity, () => now);
    await expect(engine.run({ dryRun: true })).rejects.toThrow("開始");
    await ledger.initialize(
      { ...identity(), since: new Date(now).toISOString() },
      now,
    );
    await engine.run({ dryRun: true });
    expect(api.downloaded).toEqual(["new"]);
    expect(api.sent).toHaveLength(0);
    expect((await ledger.rows())[0].status).toBe("pending");
    await expect(
      ledger.initialize({ ...identity(), since: "2020-01-01T00:00:00Z" }, now),
    ).rejects.toThrow("設定済み");
    const future = await Ledger.open(`future-${crypto.randomUUID()}`);
    await expect(
      future.initialize({ ...identity(), since: "2099-01-01T00:00:00Z" }, now),
    ).rejects.toThrow("現在以前");
  });
  it("sends once across reopen, resync, a new Graph ID and repeated MIME", async () => {
    const { ledger, api, engine } = await setup();
    await engine.run();
    expect(api.sent).toHaveLength(1);
    const name = ledger.db.name;
    ledger.db.close();
    const reopened = await Ledger.open(name);
    api.items = [item(), item("new-graph-id")];
    await new Engine(reopened, api, core, identity, () => now).run();
    expect(api.sent).toHaveLength(1);
    expect(await reopened.rows()).toHaveLength(1);
    expect((await reopened.rows())[0].raw).toBeUndefined(); // Ledger remains; confirmed originals may be released.
    expect(await reopened.seen("new-graph-id")).toBeTruthy();
  });
  it("persists a failed-send retry, observes backoff and succeeds on next run", async () => {
    const api = new FakeApi(
      [item()],
      [
        { status: "retry", error: "rate limit" },
        { status: "sent", gmailId: "ok" },
      ],
    );
    const { ledger, engine } = await setup(api);
    await engine.run();
    expect((await ledger.rows())[0].attempts).toBe(1);
    await engine.run();
    expect(api.sent).toHaveLength(1);
    await new Engine(ledger, api, core, identity, () => now + 60001).run();
    const row = (await ledger.rows())[0];
    expect(row.status).toBe("sent");
    expect(row.attempts).toBe(2);
    expect(row.gmailId).toBe("ok");
    const ids = api.sent.map(
      (m) => new TextDecoder().decode(m).match(/Message-ID: ([^\r]+)/)[1],
    );
    expect(ids[0]).toBe(ids[1]);
  });
  it("recovers interrupted sends as unknown and requires explicit duplicate-risk acknowledgment", async () => {
    const { ledger, api, engine } = await setup();
    await engine.run({ dryRun: true });
    const key = (await ledger.rows())[0].key;
    await ledger.update(key, (r) => ({ ...r, status: "sending", attempts: 1 }));
    await engine.run();
    expect(api.sent).toHaveLength(0);
    expect((await ledger.rows())[0].status).toBe("unknown");
    await expect(ledger.retry(key)).rejects.toThrow("重複リスク");
    await ledger.retry(key, true);
    await engine.run();
    expect(api.sent).toHaveLength(1);
    await expect(ledger.retry(key, true)).rejects.toThrow("再試行できません");
  });
  it("does not advance a page after an import failure, and safely replays imported items", async () => {
    const api = new FakeApi([item("one"), item("two")]);
    const { ledger, engine } = await setup(api);
    const mime = api.mime.bind(api);
    let fail = true;
    api.mime = async (id) => {
      if (id === "two" && fail) throw new ApiError("GRAPH", "temporary");
      return mime(id);
    };
    await expect(engine.run({ dryRun: true })).rejects.toThrow("temporary");
    expect(await ledger.get("cursor")).toBeUndefined();
    fail = false;
    await engine.run({ dryRun: true });
    expect(api.downloaded.filter((id) => id === "one")).toHaveLength(1);
    expect(await ledger.get("cursor")).toBe(delta);
  });
  it("resets expired delta and deduplicates replay without changing the initial boundary", async () => {
    const { ledger, api, engine } = await setup();
    await engine.run();
    const pageReader = api.page.bind(api);
    let expire = true;
    api.page = async (url) => {
      if (url === delta && expire) {
        expire = false;
        throw new ApiError("EXPIRED", "expired");
      }
      return pageReader(url);
    };
    await engine.run();
    expect(api.sent).toHaveLength(1);
    expect((await ledger.get("start")).since).toBe(new Date(now).toISOString());
  });
  it("refuses switched accounts and concurrent tabs, and persists intent before POST", async () => {
    const { ledger, api } = await setup();
    await expect(
      new Engine(ledger, api, core, () => ({
        ...identity(),
        gmail: "other@gmail.com",
      })).run(),
    ).rejects.toThrow("異なるアカウント");
    await expect(
      exclusive(
        () => {
          throw new Error("must not run");
        },
        { request: (_name, _options, cb) => cb(null) },
      ),
    ).rejects.toThrow("別のタブ");
    const engine = new Engine(ledger, api, core, identity, () => now);
    await engine.run({ dryRun: true });
    const update = ledger.update.bind(ledger);
    ledger.update = async (key, fn) => {
      const row = (await ledger.rows()).find((r) => r.key === key);
      if (fn(row).status === "sending") throw new Error("storage full");
      return update(key, fn);
    };
    await expect(engine.forward()).rejects.toThrow("storage full");
    expect(api.sent).toHaveLength(0);
  });
  it("holds an oversized original without dropping its identity or sending partial attachments", async () => {
    const { ledger, api, engine } = await setup();
    api.mime = async () => {
      throw new ApiError("SIZE", "oversize");
    };
    await engine.run();
    const row = (await ledger.rows())[0];
    expect(row.status).toBe("blocked");
    expect(await ledger.seen("graph-1")).toBeTruthy();
    expect(api.sent).toHaveLength(0);
    await expect(ledger.retry(row.key)).rejects.toThrow("サイズ");
  });
});
describe("HTTP classification without real mail or hidden retries", () => {
  const auth = {
    googleToken: () => "fake-not-a-real-token",
    microsoftToken: () => "fake-not-a-real-token",
  };
  it.each([429, 401, 403, 408, 500, 503])(
    "classifies HTTP %s and submits only once",
    async (status) => {
      const fetcher = vi.fn(async () => new Response("{}", { status }));
      const result = await new MailApi(auth, fetcher).send(raw());
      expect(result.status).toBe(
        status === 429
          ? "retry"
          : status === 408 || status >= 500
            ? "unknown"
            : "blocked",
      );
      expect(fetcher).toHaveBeenCalledTimes(1);
    },
  );
  it("handles ambiguous success, network reset, expiry before POST and exact Gmail payload", async () => {
    const bad = new MailApi(auth, async () => new Response("not JSON"));
    expect((await bad.send(raw())).status).toBe("unknown");
    const reset = new MailApi(auth, async () => {
      throw new TypeError("reset");
    });
    expect((await reset.send(raw())).status).toBe("unknown");
    const fetcher = vi.fn(async (_url, options) => {
      expect(JSON.parse(options.body).raw).toBe(base64url(raw()));
      return new Response('{"id":"fake-sent"}');
    });
    expect((await new MailApi(auth, fetcher).send(raw())).status).toBe("sent");
    const expired = new MailApi(
      {
        googleToken: () => {
          throw new ApiError("AUTH", "expired");
        },
      },
      fetcher,
    );
    await expect(expired.send(raw())).rejects.toThrow("expired");
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
  it("rejects foreign delta URLs before bearer credentials are sent", () => {
    expect(() =>
      graphLink(
        "https://evil.example/v1.0/me/mailFolders/inbox/messages/delta",
      ),
    ).toThrow();
    expect(() =>
      graphLink(
        "https://graph.microsoft.com/v1.0/users/other/mailFolders/inbox/messages/delta",
      ),
    ).toThrow();
    expect(() =>
      graphLink(
        "https://user:pass@graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta",
      ),
    ).toThrow();
    expect(decodeURIComponent(firstPage("2026-10-01T00:00:00Z"))).toContain(
      "receivedDateTime+ge+2026-10-01T00:00:00.000Z",
    );
  });
});

it("stops between messages, holds invalid MIME, and prevents resending after success-save failure", async () => {
  const api = new FakeApi([item("first"), item("second")]);
  api.mime = async (id) => raw(`${id}@example.com`);
  const { ledger, engine } = await setup(api);
  await engine.run({ dryRun: true });
  const send = api.send.bind(api);
  api.send = async (mime) => {
    engine.stop();
    return send(mime);
  };
  await engine.forward();
  expect(api.sent).toHaveLength(1);
  expect(
    (await ledger.rows()).filter((r) => r.status === "pending"),
  ).toHaveLength(1);

  const invalid = await setup();
  invalid.api.mime = async () => new Uint8Array();
  await invalid.engine.run();
  expect((await invalid.ledger.rows())[0].status).toBe("blocked");
  expect(invalid.api.sent).toHaveLength(0);

  const crashed = await setup();
  await crashed.engine.run({ dryRun: true });
  const update = crashed.ledger.update.bind(crashed.ledger);
  crashed.ledger.update = async (key, fn) => {
    const row = (await crashed.ledger.rows()).find((r) => r.key === key);
    if (fn(row).status === "sent") throw new Error("commit failure");
    return update(key, fn);
  };
  await expect(crashed.engine.forward()).rejects.toThrow("commit failure");
  crashed.ledger.update = update;
  await crashed.engine.run();
  expect(crashed.api.sent).toHaveLength(1);
  expect((await crashed.ledger.rows())[0].status).toBe("unknown");
});
