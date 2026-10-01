import {
  PublicClientApplication,
  BrowserCacheLocation,
  LogLevel,
} from "@azure/msal-browser";
import { ApiError } from "./api.js";
const sendScope = "https://www.googleapis.com/auth/gmail.send";
const emailScope = "https://www.googleapis.com/auth/userinfo.email";
export class Auth {
  constructor(fetcher = fetch) {
    this.fetch = fetcher;
    this.google = null;
    this.microsoft = null;
  }
  async configure(settings) {
    this.google = null;
    this.microsoft = null;
    if (
      !settings.googleId.endsWith(".apps.googleusercontent.com") ||
      !/^[0-9a-f-]{36}$/i.test(settings.microsoftId)
    )
      throw new Error(
        "公開client IDの形式を確認してください。secretは入力しません。",
      );
    this.settings = settings;
    this.msal = new PublicClientApplication({
      auth: {
        clientId: settings.microsoftId,
        authority: "https://login.microsoftonline.com/organizations",
        redirectUri: new URL("redirect.html", location.href).href,
      },
      cache: { cacheLocation: BrowserCacheLocation.MemoryStorage },
      system: {
        loggerOptions: {
          loggerCallback: () => {},
          piiLoggingEnabled: false,
          logLevel: LogLevel.Error,
        },
      },
    });
    await this.msal.initialize();
    if (!globalThis.google?.accounts?.oauth2)
      await new Promise((resolve, reject) => {
        const script = document.createElement("script");
        script.src = "https://accounts.google.com/gsi/client";
        script.async = true;
        script.onload = resolve;
        script.onerror = () =>
          reject(
            new Error(
              "Googleのログイン画面を読み込めませんでした。接続を確認してください。",
            ),
          );
        document.head.append(script);
      });
  }
  connectGoogle() {
    this.google = null;
    return new Promise((resolve, reject) => {
      const client = globalThis.google.accounts.oauth2.initTokenClient({
        client_id: this.settings.googleId,
        scope: `${sendScope} openid ${emailScope}`,
        include_granted_scopes: false,
        callback: async (response) => {
          try {
            if (
              response.error ||
              !globalThis.google.accounts.oauth2.hasGrantedAllScopes(
                response,
                sendScope,
                "openid",
                emailScope,
              )
            )
              throw new Error("Gmailの送信権限への同意が必要です。");
            const expires = Date.now() + Number(response.expires_in) * 1000;
            const r = await this.fetch(
              "https://openidconnect.googleapis.com/v1/userinfo",
              {
                headers: { Authorization: `Bearer ${response.access_token}` },
                redirect: "error",
                signal: AbortSignal.timeout(30000),
              },
            );
            if (!r.ok)
              throw new Error("Gmailアカウントを確認できませんでした。");
            const user = await r.json();
            if (
              !user.email_verified ||
              !/^[a-z0-9._+-]+@gmail\.com$/i.test(user.email || "") ||
              !Number.isFinite(expires)
            )
              throw new Error("確認済みの個人Gmailでログインしてください。");
            this.google = {
              email: user.email.toLowerCase(),
              token: response.access_token,
              expires,
            };
            resolve();
          } catch {
            reject(
              new Error(
                "Gmailを接続できませんでした。アプリ登録、テストユーザー、送信権限を確認してください。",
              ),
            );
          }
        },
        error_callback: () =>
          reject(
            new Error(
              "Googleのログイン画面が閉じられたか、ポップアップを開けませんでした。",
            ),
          ),
      });
      client.requestAccessToken({ prompt: "select_account" });
    });
  }
  async connectMicrosoft() {
    this.microsoft = null;
    // Called directly from a button: popup is opened before awaiting any other work.
    const result = await this.msal.acquireTokenPopup({
      scopes: ["User.Read", "Mail.Read"],
      prompt: "select_account",
    });
    const r = await this.fetch(
      "https://graph.microsoft.com/v1.0/me?$select=id,mail,userPrincipalName",
      {
        headers: { Authorization: `Bearer ${result.accessToken}` },
        redirect: "error",
        signal: AbortSignal.timeout(30000),
      },
    );
    if (!r.ok) throw new Error("大学アカウントを確認できませんでした。");
    const user = await r.json();
    const email = [user.mail, user.userPrincipalName].find(
      (v) => typeof v === "string" && /^[^\s@]+@hiroshima-u\.ac\.jp$/i.test(v),
    );
    if (!email || typeof user.id !== "string" || !user.id)
      throw new Error("広島大学のMicrosoftアカウントでログインしてください。");
    this.microsoft = {
      email: email.toLowerCase(),
      id: user.id,
      token: result.accessToken,
      expires: result.expiresOn?.getTime() || 0,
    };
  }
  identity() {
    return { gmail: this.google?.email, microsoftId: this.microsoft?.id };
  }
  googleToken() {
    if (!this.google || this.google.expires < Date.now() + 60000)
      throw new ApiError(
        "AUTH",
        "Gmailに再接続してください。画面を閉じるとログイン状態は消えます。",
      );
    return this.google.token;
  }
  microsoftToken() {
    if (!this.microsoft || this.microsoft.expires < Date.now() + 60000)
      throw new ApiError("AUTH", "大学メールに再接続してください。");
    return this.microsoft.token;
  }
  async disconnect() {
    this.google = null;
    this.microsoft = null;
    await this.msal?.clearCache();
  }
}
