// The notification bell. Push can't work without a script, so the button is
// rendered hidden and only shown here, once the browser and the server can
// both do push.

const bell = document.getElementById("push-bell");

if (bell && "serviceWorker" in navigator && "PushManager" in window) {
  setUp(bell).catch((error) => console.error("Notifications:", error));
}

async function setUp(bell) {
  const response = await fetch("/push/vapid-public-key");
  if (!response.ok) return; // Not set up on the server.
  const { publicKey } = await response.json();
  const registration = await navigator.serviceWorker.register("/sw.js");

  const show = (state) => {
    bell.dataset.state = state;
    bell.querySelector("span").textContent =
      { on: "On", off: "Off", blocked: "Blocked" }[state];
    bell.disabled = state === "blocked";
    bell.hidden = false;
  };

  const subscription = await registration.pushManager.getSubscription();
  if (Notification.permission === "denied") show("blocked");
  else show(subscription ? "on" : "off");

  bell.addEventListener("click", async () => {
    bell.disabled = true;
    try {
      const current = await registration.pushManager.getSubscription();
      if (current) {
        await send("DELETE", { endpoint: current.endpoint });
        await current.unsubscribe();
        show("off");
      } else {
        const permission = await Notification.requestPermission();
        if (permission !== "granted") {
          // Dismissing the prompt isn't a refusal; it can be asked again.
          show(permission === "denied" ? "blocked" : "off");
          return;
        }
        const created = await registration.pushManager.subscribe({
          userVisibleOnly: true,
          applicationServerKey: fromBase64Url(publicKey),
        });
        await send("POST", created.toJSON());
        show("on");
      }
    } catch (error) {
      console.error("Notifications:", error);
    } finally {
      bell.disabled = bell.dataset.state === "blocked";
    }
  });
}

async function send(method, body) {
  const response = await fetch("/push/subscribe", {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(`${method} /push/subscribe: ${response.status}`);
}

function fromBase64Url(text) {
  const base64 = text.replace(/-/g, "+").replace(/_/g, "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
}
