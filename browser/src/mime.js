import init, {
  inspect_mime,
  compose_mime,
  forward_id,
} from "../pkg/hiro_mail_core.js";
export async function loadCore() {
  await init();
  return {
    inspect: (raw) => JSON.parse(inspect_mime(raw)),
    forwardId: forward_id,
    compose: (raw, gmail, milliseconds) => {
      const nonce = Array.from(
        crypto.getRandomValues(new Uint8Array(24)),
        (b) => b.toString(16).padStart(2, "0"),
      ).join("");
      return compose_mime(
        raw,
        gmail,
        nonce,
        BigInt(Math.floor(milliseconds / 1000)),
      );
    },
  };
}
