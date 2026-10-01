import { Ledger, exclusive } from "./ledger.js";
import { Auth } from "./auth.js";
import { MailApi } from "./api.js";
import { Engine } from "./engine.js";
import { loadCore } from "./mime.js";
const $ = (id) => document.getElementById(id);
const notice = (text, error = false) => {
  $("notice").textContent = text;
  $("notice").classList.toggle("error", error);
};
const labels = {
  pending: "転送待ち",
  retry: "再試行待ち",
  sending: "送信中",
  blocked: "保留",
  unknown: "結果不明",
  sent: "転送済み",
};
let ledger,
  core,
  engine,
  busy = false,
  ready = false;
const auth = new Auth();
const displayDate = (value) =>
  value ? new Date(value).toLocaleString("ja-JP") : "未同期";
async function render() {
  const start = await ledger.get("start");
  const rows = await ledger.rows();
  $("google-state").textContent = `Gmail：${auth.google?.email || "未接続"}`;
  $("microsoft-state").textContent =
    `広大メール：${auth.microsoft?.email || "未接続"}`;
  for (const id of ["google", "microsoft"]) $(id).disabled = busy || !ready;
  $("disconnect").disabled = busy || !ready;
  $("save-settings").disabled = busy;
  $("initialize").disabled = busy || !!start || !auth.google || !auth.microsoft;
  $("start-controls").hidden = !!start;
  $("start-value").textContent = start
    ? `${displayDate(start.since)} 以降の受信分 → ${start.gmail}`
    : "未設定です。過去のメールを勝手に転送しません。";
  const match =
    start &&
    start.gmail === auth.google?.email &&
    start.microsoftId === auth.microsoft?.id;
  $("preview").disabled = busy || !match;
  $("send").disabled = busy || !match || !(await ledger.get("lastSync"));
  $("stop").disabled = !busy || !engine;
  $("state").textContent = busy ? "処理中" : "停止中";
  $("state").classList.toggle("running", busy);
  $("last-sync").textContent =
    `最後の同期：${displayDate(await ledger.get("lastSync"))}。1回に最大10ページ・50通を処理します。`;
  $("pending-count").textContent = rows.filter((r) =>
    ["pending", "retry"].includes(r.status),
  ).length;
  $("sent-count").textContent = rows.filter((r) => r.status === "sent").length;
  $("held-count").textContent = rows.filter((r) =>
    ["blocked", "unknown", "sending"].includes(r.status),
  ).length;
  const container = $("messages");
  container.replaceChildren();
  for (const row of rows
    .sort((a, b) => Date.parse(b.receivedAt) - Date.parse(a.receivedAt))
    .slice(0, 50)) {
    // Mail metadata is untrusted. Do not render original HTML or interpolate it into markup.
    const div = document.createElement("div");
    div.className = "message";
    const title = document.createElement("strong");
    title.textContent = row.subject;
    const status = document.createElement("span");
    status.className = "tag";
    status.textContent = labels[row.status];
    const from = document.createElement("p");
    from.textContent = `${row.sender_address} · 受信 ${displayDate(row.receivedAt)} · 試行 ${row.attempts} 回`;
    div.append(status, title, from);
    if (row.error) {
      const p = document.createElement("p");
      p.textContent = row.error;
      div.append(p);
    }
    if (row.status === "retry") {
      const p = document.createElement("p");
      p.textContent = `次の再試行：${displayDate(row.nextAt)} 以降に「同期して転送」`;
      div.append(p);
    }
    if (row.status === "unknown" || row.status === "sending") {
      const p = document.createElement("p");
      p.textContent = "Gmailの検索欄で受信済みか確認：";
      const code = document.createElement("code");
      code.textContent = `in:anywhere rfc822msgid:${row.forwardId.replace(/^<|>$/g, "")}`;
      div.append(p, code);
    }
    if (["unknown", "blocked"].includes(row.status) && row.raw) {
      const button = document.createElement("button");
      button.className = "secondary";
      button.disabled = busy;
      button.textContent =
        row.status === "unknown"
          ? "Gmail確認後に再試行を予約"
          : "接続確認後に再試行を予約";
      button.onclick = () =>
        action(async () => {
          if (
            row.status === "unknown" &&
            !confirm(
              "Gmailで受信済みか確認しましたか？送信結果が不明なため、再送すると重複する可能性があります。了承して再試行を予約します。",
            )
          )
            return;
          await exclusive(() =>
            ledger.retry(row.key, row.status === "unknown"),
          );
          notice("再試行を予約しました。「同期して転送」で実行できます。");
        });
      div.append(button);
    }
    container.append(div);
  }
  if (!rows.length) {
    const p = document.createElement("p");
    p.className = "empty";
    p.textContent =
      "転送対象はまだありません。開始位置を設定し、対象を確認してください。";
    container.append(p);
  }
}
async function action(
  operation,
  fallback = "操作を完了できませんでした。接続と保存設定を確認してください。",
) {
  busy = true;
  for (const button of document.querySelectorAll("button"))
    button.disabled = true;
  try {
    await operation();
  } catch (e) {
    notice(e instanceof Error && !e.errorCode ? e.message : fallback, true);
  } finally {
    busy = false;
    engine = null;
    await render();
  }
}
$("save-settings").onclick = () =>
  action(async () => {
    const settings = {
      googleId: $("google-id").value.trim(),
      microsoftId: $("microsoft-id").value.trim(),
    };
    ready = false;
    await auth.configure(settings);
    await ledger.set("clients", settings);
    ready = true;
    $("settings").open = false;
    notice("ログイン設定を保存しました。両方のアカウントを接続してください。");
  });
$("google").onclick = () =>
  action(async () => {
    await auth.connectGoogle();
    notice("Gmailを接続しました。");
  }, "Gmailに接続できませんでした。");
$("microsoft").onclick = () =>
  action(async () => {
    await auth.connectMicrosoft();
    notice("大学メールを接続しました。");
  }, "大学メールに接続できませんでした。アプリ登録・大学の管理者同意を確認してください。");
$("disconnect").onclick = () =>
  action(async () => {
    await auth.disconnect();
    notice("この画面の接続を解除しました。転送履歴は保持しています。");
  });
for (const radio of document.querySelectorAll('input[name="start"]'))
  radio.onchange = () => {
    $("since").disabled = radio.value !== "since";
  };
$("initialize").onclick = () =>
  action(async () => {
    const now = Date.now();
    const since =
      document.querySelector('input[name="start"]:checked').value === "now"
        ? new Date(now).toISOString()
        : new Date($("since").value).toISOString();
    await exclusive(() =>
      ledger.initialize({ ...auth.identity(), since }, now),
    );
    await navigator.storage?.persist?.();
    notice(
      "開始位置を保存しました。「対象を確認（送信なし）」でメールを確認してください。",
    );
  });
function run(dryRun) {
  return action(async () => {
    await exclusive(async () => {
      if (!dryRun && !(await ledger.get("lastSync")))
        throw new Error("先に送信なしで対象を確認してください。");
      engine = new Engine(ledger, new MailApi(auth), core, () =>
        auth.identity(),
      );
      await render();
      notice(
        dryRun
          ? "大学メールを確認しています。Gmailへは送信しません。"
          : "同期と転送を開始しました。この画面を開いたままお待ちください。",
      );
      const result = await engine.run({ dryRun, progress: notice });
      const pending = (await ledger.rows()).filter((r) =>
        ["pending", "retry"].includes(r.status),
      ).length;
      notice(
        `${dryRun ? "送信なしの確認" : "同期と転送"}が終了しました。新規保存 ${result.imported} 件${dryRun ? "" : `・転送 ${result.sent || 0} 件`}・転送待ち ${pending} 件。${result.finished ? "" : "続きは次の同期で取得します。"}`,
      );
    });
  });
}
$("preview").onclick = () => run(true);
$("send").onclick = () => run(false);
$("stop").onclick = () => {
  engine?.stop();
  notice(
    "現在の通信が終わったら停止します。送信中の画面終了は結果不明になります。",
  );
};
window.addEventListener("beforeunload", (event) => {
  if (busy && engine) {
    event.preventDefault();
    event.returnValue = "";
  }
});
try {
  [ledger, core] = await Promise.all([Ledger.open(), loadCore()]);
  await exclusive(() => ledger.recover());
  const settings = await ledger.get("clients");
  if (settings) {
    $("google-id").value = settings.googleId;
    $("microsoft-id").value = settings.microsoftId;
    await auth.configure(settings);
    ready = true;
    notice(
      "アカウントに接続して同期してください。画面を開くだけでは送信しません。",
    );
  } else {
    $("settings").open = true;
    notice(
      "最初の1回だけ「初回のログイン設定」を済ませて、アカウントを接続してください。",
    );
  }
  await render();
} catch {
  notice(
    "画面を準備できませんでした。通常のブラウザーで開き、サイトデータの保存と通信を許可してください。",
    true,
  );
}
