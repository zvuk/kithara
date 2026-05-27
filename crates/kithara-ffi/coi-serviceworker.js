if (typeof window === "undefined") {
  self.addEventListener("install", () => self.skipWaiting());
  self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));
  self.addEventListener("message", (event) => {
    if (event.data && event.data.type === "deregister") {
      self.registration
        .unregister()
        .then(() => self.clients.matchAll())
        .then((clients) => {
          clients.forEach((client) => client.navigate(client.url));
        });
    }
  });
  self.addEventListener("fetch", (event) => {
    const request = event.request;
    if (request.cache === "only-if-cached" && request.mode !== "same-origin") {
      return;
    }
    event.respondWith(
      fetch(request)
        .then((response) => {
          if (
            response.status === 0 ||
            response.type === "opaqueredirect" ||
            !response.body
          ) {
            return response;
          }

          const headers = new Headers(response.headers);
          headers.set("Cross-Origin-Embedder-Policy", "require-corp");
          if (self.registration.scope.endsWith("-credentialless/")) {
            headers.set("Cross-Origin-Embedder-Policy", "credentialless");
          }
          headers.set("Cross-Origin-Opener-Policy", "same-origin");
          return new Response(response.body, {
            status: response.status,
            statusText: response.statusText,
            headers,
          });
        })
        .catch((error) => console.error(error)),
    );
  });
} else {
  if (window?.crossOriginIsolated !== false || !window.isSecureContext) {
    // Already isolated, or not secure context.
  } else {
    const navigatorRef = window.navigator;
    const documentRef = window.document;
    const storage = window.sessionStorage;
    const coi = window.coi = {
      shouldRegister() {
        return !navigatorRef.serviceWorker.controller;
      },
      shouldDeregister() {
        return false;
      },
      doReload() {
        window.location.reload();
      },
      quiet: false,
      ...window.coi,
    };

    const log = coi.quiet
      ? () => {}
      : (...args) => console.log(...args);

    if (coi.shouldDeregister()) {
      navigatorRef.serviceWorker.ready.then((registration) => {
        registration.active.postMessage({ type: "deregister" });
      });
      window.coi = coi;
    } else if (coi.shouldRegister()) {
      navigatorRef.serviceWorker
        .register(documentRef.currentScript.src)
        .then((registration) => {
          log("COOP/COEP Service Worker registered", registration.scope);
          if (registration.active && !navigatorRef.serviceWorker.controller) {
            if (window.sessionStorage.getItem("coiReloadedBySelf")) {
              log("COOP/COEP Service Worker failed to control page.");
              window.sessionStorage.removeItem("coiReloadedBySelf");
            } else {
              log("Reloading page to make use of COOP/COEP Service Worker.");
              window.sessionStorage.setItem("coiReloadedBySelf", "true");
              coi.doReload();
            }
          }
        })
        .catch((error) => {
          log("COOP/COEP Service Worker failed to register:", error);
        });
      window.addEventListener("beforeunload", () => {
        storage.setItem("coiReloadedBySelf", "true");
      });
      navigatorRef.serviceWorker.addEventListener("controllerchange", () => {
        if (storage.getItem("coiReloadedBySelf")) {
          storage.removeItem("coiReloadedBySelf");
          coi.doReload();
        }
      });
    }
    window.coi = coi;
  }
}
