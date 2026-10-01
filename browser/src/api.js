export class ApiError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}
export function graphLink(value) {
  const u = new URL(value);
  if (
    u.origin !== "https://graph.microsoft.com" ||
    u.username ||
    u.password ||
    u.hash ||
    !/^\/v1\.0\/me\/mailFolders(?:\/|\()/.test(u.pathname) ||
    !u.pathname.endsWith("/messages/delta")
  ) {
    throw new ApiError("LINK", "同期位置が不正です。");
  }
  return u.href;
}
export function firstPage(since) {
  const u = new URL(
    "https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta",
  );
  u.searchParams.set(
    "$select",
    "id,receivedDateTime,subject,from,hasAttachments,internetMessageId",
  );
  u.searchParams.set(
    "$filter",
    `receivedDateTime ge ${new Date(since).toISOString()}`,
  );
  return u.href;
}
export async function limitedBytes(response, cap) {
  if (Number(response.headers.get("content-length")) > cap)
    throw new ApiError("SIZE", "原本のサイズ上限（18 MiB）を超えています。");
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > cap)
        throw new ApiError("SIZE", "取得サイズの上限を超えています。");
      chunks.push(value);
    }
  } finally {
    await reader.cancel().catch(() => {});
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const c of chunks) {
    bytes.set(c, offset);
    offset += c.length;
  }
  return bytes;
}
export function base64url(bytes) {
  let text = "";
  for (let i = 0; i < bytes.length; i += 8192)
    text += String.fromCharCode(...bytes.subarray(i, i + 8192));
  return btoa(text)
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");
}
export class MailApi {
  constructor(auth, fetcher = fetch) {
    this.auth = auth;
    this.fetch = fetcher;
  }
  async graph(url, cap) {
    let response;
    try {
      response = await this.fetch(url, {
        headers: {
          Authorization: `Bearer ${this.auth.microsoftToken()}`,
          Prefer: 'IdType="ImmutableId", odata.maxpagesize=50',
        },
        redirect: "error",
        signal: AbortSignal.timeout(60000),
      });
    } catch (e) {
      if (e instanceof ApiError) throw e;
      throw new ApiError(
        "GRAPH",
        "大学メールを取得できませんでした。接続を確認して再実行してください。",
      );
    }
    if (response.status === 410)
      throw new ApiError("EXPIRED", "同期位置が期限切れです。");
    if (response.status === 404)
      throw new ApiError(
        "MISSING",
        "大学側で移動・削除されたメールは取得できません。",
      );
    if (!response.ok)
      throw new ApiError(
        "GRAPH",
        `大学メールの取得が拒否されました（HTTP ${response.status}）。再接続または同意設定を確認してください。`,
      );
    return limitedBytes(response, cap);
  }
  async page(url) {
    const bytes = await this.graph(graphLink(url), 2 * 1024 * 1024);
    let page;
    try {
      page = JSON.parse(new TextDecoder().decode(bytes));
    } catch {
      throw new ApiError("PAGE", "大学メールの一覧応答が不正です。");
    }
    if (
      !Array.isArray(page.value) ||
      (!page["@odata.nextLink"] && !page["@odata.deltaLink"])
    )
      throw new ApiError("PAGE", "大学メールの同期応答が不正です。");
    if (page["@odata.nextLink"]) graphLink(page["@odata.nextLink"]);
    if (page["@odata.deltaLink"]) graphLink(page["@odata.deltaLink"]);
    return page;
  }
  mime(id) {
    return this.graph(
      `https://graph.microsoft.com/v1.0/me/messages/${encodeURIComponent(id)}/$value`,
      18 * 1024 * 1024,
    );
  }
  // No hidden retries: any fetch error might occur AFTER Gmail accepts a message.
  async send(mime) {
    const token = this.auth.googleToken(); // An expired token is detected before submission.
    let r;
    try {
      r = await this.fetch(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/send",
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${token}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ raw: base64url(mime) }),
          redirect: "error",
          signal: AbortSignal.timeout(60000),
        },
      );
    } catch {
      return {
        status: "unknown",
        error: "通信が中断しました。Gmailに届いている可能性があります。",
      };
    }
    if (r.status === 429)
      return {
        status: "retry",
        error: "送信制限です。時間を置いて再実行してください。",
      };
    if (r.status === 408 || r.status >= 500)
      return {
        status: "unknown",
        error: `送信結果が不明です（HTTP ${r.status}）。Gmailを確認してください。`,
      };
    if (!r.ok)
      return {
        status: "blocked",
        error: `送信が拒否されました（HTTP ${r.status}）。接続・権限を確認してください。`,
      };
    try {
      const body = JSON.parse(
        new TextDecoder().decode(await limitedBytes(r, 1024 * 1024)),
      );
      if (typeof body.id === "string" && body.id)
        return { status: "sent", gmailId: body.id };
    } catch {
      /* accepted but unusable response */
    }
    return {
      status: "unknown",
      error: "成功応答を確認できませんでした。Gmailを確認してください。",
    };
  }
}
