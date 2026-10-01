"use strict";
const $ = (id) => document.getElementById(id);
let state = { authenticated: false },
  busy = false;
const labels = {
  sent: "転送済み",
  pending: "待機中",
  retry: "再試行待ち",
  blocked: "送信保留",
  unknown: "結果不明",
  sending: "送信中",
};
function notice(message, error = false) {
  $("notice").textContent = message;
  $("notice").classList.toggle("error", error);
  $("notice").hidden = !message;
}
async function api(path, body) {
  const options = { credentials: "same-origin", headers: {} };
  if (body !== undefined) {
    options.method = "POST";
    options.headers = {
      "Content-Type": "application/json",
      "X-CSRF-Token": state.csrf || "",
    };
    options.body = JSON.stringify(body);
  }
  const response = await fetch(path, options);
  const result = await response.json();
  if (!response.ok)
    throw new Error(
      result.error || "処理に失敗しました。画面を再読み込みしてください。",
    );
  return result;
}
function time(value) {
  return value
    ? new Date(typeof value === "number" ? value * 1000 : value).toLocaleString(
        "ja-JP",
      )
    : "未同期";
}
function render() {
  const signed = state.authenticated,
    connected = signed && !!state.university;
  const settings = state.settings || {},
    started = !!settings.start_at,
    running = !!settings.enabled;
  $("gmail-state").textContent = signed
    ? state.gmail
    : "Gmailでログインして始めます。";
  $("google-login").textContent = signed ? "Gmailを再接続" : "Googleでログイン";
  $("google-login").disabled = busy || (!signed && !state.google_ready);
  $("google-help").textContent =
    !signed && !state.google_ready
      ? "管理者のGoogleログイン設定待ちです。Renderの環境変数を設定してください。"
      : "このサービスに設定された所有者だけがログインできます。";
  $("logout").hidden = !signed;
  $("logout").disabled = busy;
  $("university-state").textContent = connected
    ? state.university
    : signed
      ? "広島大学のMicrosoftアカウントを接続します。"
      : "Gmailへのログイン後に接続できます。";
  $("microsoft-login").textContent = connected
    ? "大学メールを再接続"
    : "Microsoftで接続";
  $("microsoft-login").disabled = busy || !signed || !state.microsoft_ready;
  if (signed && !state.microsoft_ready)
    $("university-state").textContent =
      "管理者のMicrosoftログイン設定待ちです。";
  $("start-form").hidden = started;
  $("start-state").hidden = !started;
  $("start-state").textContent = started
    ? `${time(settings.start_at)} 以降に受信したメール`
    : "";
  $("save-start").disabled = busy || !connected;
  $("since").disabled =
    busy ||
    document.querySelector("input[name=start]:checked").value !== "since";
  $("preview").disabled = busy || !started;
  $("toggle").disabled = busy || !started || (!running && !settings.last_sync);
  $("toggle").textContent = running ? "転送を停止" : "転送を開始";
  $("run-state").textContent = running
    ? "自動転送しています"
    : started
      ? "対象を確認してから開始"
      : "接続待ち";
  $("run-badge").textContent = running ? "稼働中" : "停止中";
  $("run-badge").classList.toggle("running", running);
  $("run-help").textContent = running
    ? "新しく受信したメールを定期的に確認し、接続したGmailへ送信します。"
    : started
      ? "「対象を確認」で大学メールを読み取り、転送予定を表示します。確認できたら転送を開始してください。"
      : "まず左の手順でアカウントを接続してください。";
  const counts = state.counts || {};
  $("sent").textContent = signed ? counts.sent || 0 : "—";
  $("pending").textContent = signed
    ? (counts.pending || 0) + (counts.retry || 0)
    : "—";
  $("held").textContent = signed
    ? (counts.blocked || 0) + (counts.unknown || 0)
    : "—";
  $("last-sync").textContent = signed
    ? `前回の同期：${time(settings.last_sync)}${settings.last_error ? " · " + settings.last_error : ""}`
    : "";
  for (const button of $("history").querySelectorAll("button"))
    button.disabled = busy;
}
function element(name, text, className) {
  const el = document.createElement(name);
  el.textContent = text;
  if (className) el.className = className;
  return el;
}
function messages(id, rows, empty, history = false) {
  const container = $(id);
  container.replaceChildren();
  if (!rows.length) {
    container.append(element("p", empty, "empty"));
    return;
  }
  for (const row of rows) {
    const item = element("article", "", "message");
    item.append(element("strong", row.subject || `メール #${row.id}`));
    if (history) {
      item.append(
        element(
          "p",
          `${labels[row.status] || row.status}${row.error_code ? " · " + row.error_code : ""}`,
        ),
      );
      if (["retry", "blocked", "unknown"].includes(row.status)) {
        if (row.status === "unknown" && row.forwarding_message_id) {
          item.append(element("p", "Gmailの検索欄に貼り付け："));
          item.append(
            element(
              "code",
              "in:anywhere rfc822msgid:" +
                row.forwarding_message_id.replace(/^<|>$/g, ""),
            ),
          );
        }
        const button = element(
          "button",
          row.status === "unknown" ? "確認して再試行" : "再試行を予約",
          "secondary",
        );
        button.disabled = busy;
        button.addEventListener("click", () =>
          action(async () => {
            const unknown = row.status === "unknown";
            if (
              unknown &&
              !window.confirm(
                "Gmailで受信済みか確認しましたか？送信結果が不明なため、再試行すると同じメールが届く可能性があります。重複リスクを了承して再試行します。",
              )
            )
              return;
            await api("/api/retry", {
              id: row.id,
              accept_duplicate_risk: unknown,
            });
            notice("再試行を予約しました。転送が有効なときに送信します。");
          }),
        );
        item.append(button);
      }
    } else {
      item.append(
        element(
          "p",
          `${row.sender || "送信者不明"} · 元日時 ${time(row.original_date)}`,
        ),
      );
      for (const filename of row.attachment_names)
        item.append(element("span", filename, "tag"));
      item.append(element("span", "original.eml", "tag"));
    }
    container.append(item);
  }
}
async function refresh() {
  state = await api("/api/status");
  render();
  if (state.authenticated && state.settings.start_at) {
    const data = await api("/api/preview");
    messages("targets", data.targets, "現在送信できるメールはありません。");
    messages("history", data.history, "転送履歴はまだありません。", true);
  }
  if (!state.authenticated) {
    $("targets").replaceChildren(
      element("p", "ログインすると転送対象を確認できます。", "empty"),
    );
    $("history").replaceChildren(
      element("p", "ログインすると転送履歴を確認できます。", "empty"),
    );
  }
}
async function action(task) {
  if (busy) return;
  busy = true;
  render();
  try {
    await task();
    await refresh();
  } catch (error) {
    notice(error.message, true);
  } finally {
    busy = false;
    render();
  }
}
for (const provider of ["google", "microsoft"])
  $(
    provider === "google" ? "google-login" : "microsoft-login",
  ).addEventListener("click", () =>
    action(async () => {
      const result = await api(`/auth/${provider}/start`, {});
      window.location.assign(result.url);
    }),
  );
$("logout").addEventListener("click", () =>
  action(async () => {
    await api("/api/logout", {});
    notice("ログアウトしました。転送の開始・停止設定は保持されます。");
  }),
);
for (const radio of document.querySelectorAll("input[name=start]"))
  radio.addEventListener("change", render);
$("save-start").addEventListener("click", () =>
  action(async () => {
    const mode = document.querySelector("input[name=start]:checked").value;
    const input = { mode };
    if (mode === "since") {
      const date = new Date($("since").value);
      if (Number.isNaN(date.valueOf()))
        throw new Error("開始日時を指定してください。");
      input.since = date.toISOString();
    }
    await api("/api/start", input);
    notice("開始位置を保存しました。次に転送対象を確認してください。");
  }),
);
$("preview").addEventListener("click", () =>
  action(async () => {
    notice("大学メールを確認しています。メールは送信しません。");
    await api("/api/sync", {});
    notice("対象を確認しました。この操作ではメールを送信していません。");
  }),
);
$("toggle").addEventListener("click", () =>
  action(async () => {
    const enabled = !state.settings.enabled;
    await api("/api/enabled", { enabled });
    notice(
      enabled
        ? "自動転送を開始しました。次の定期確認から送信します。"
        : "自動転送を停止しました。処理中の1通は完了する場合があります。",
    );
  }),
);
const result = new URLSearchParams(window.location.search).get("notice");
if (result) {
  notice(
    result === "connected"
      ? "アカウントを接続しました。"
      : "接続できませんでした。正しいアカウント、アプリの設定、同意した権限を確認してください。大学側で管理者の承認が必要な場合もあります。",
    result !== "connected",
  );
  window.history.replaceState(null, "", "/");
}
refresh().catch((error) => notice(error.message, true));
setInterval(() => {
  if (!busy && !document.hidden)
    refresh().catch((error) => notice(error.message, true));
}, 15000);
