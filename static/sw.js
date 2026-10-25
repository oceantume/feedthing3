"use strict";

// Only here for push notifications: no caching, no offline pages.

self.addEventListener("push", (event) => {
  let payload = {};
  try {
    payload = event.data ? event.data.json() : {};
  } catch {
    // An unreadable message still gets a notification, just a plain one.
  }
  event.waitUntil(
    self.registration.showNotification(payload.title || "Feedthing", {
      body: payload.body || "",
      icon: "/static/favicon.svg",
      data: { url: payload.url },
    }),
  );
});

// Opens the item if the notification was about one, the summary otherwise,
// in a tab that's already open when there is one.
self.addEventListener("notificationclick", (event) => {
  const url = event.notification.data?.url || "/";
  event.notification.close();
  event.waitUntil(
    self.clients
      .matchAll({ type: "window", includeUncontrolled: true })
      .then((windows) => {
        const open = windows.find((client) => "focus" in client);
        if (!open) return self.clients.openWindow(url);
        return open.navigate(url).then((client) => (client || open).focus());
      })
      // A tab the worker doesn't control can't be sent elsewhere.
      .catch(() => self.clients.openWindow(url)),
  );
});
