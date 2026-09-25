if (typeof window === "undefined") {
  self.addEventListener("install", () => self.skipWaiting());
  self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));
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
  // Headers apply to a document when it is fetched, so a page the worker only
  // started controlling after its own load — the first visit, when the worker
  // claims it, or a hard reload, which bypasses the worker — stays
  // unisolated until it loads again. This script is the one owner of that
  // reload, and the session flag keeps it to one per navigation.
  const storage = window.sessionStorage;
  const reloadedBySelf = storage.getItem("coiReloadedBySelf") !== null;
  storage.removeItem("coiReloadedBySelf");

  if (window.crossOriginIsolated !== false || !window.isSecureContext) {
    // Already isolated, or no service worker can isolate this page.
  } else if (!("serviceWorker" in window.navigator)) {
    console.log("COOP/COEP Service Worker is not supported by this browser.");
  } else {
    const serviceWorker = window.navigator.serviceWorker;
    let reloading = false;
    const reloadOnce = () => {
      if (reloading) {
        return;
      }
      reloading = true;
      if (reloadedBySelf) {
        console.log("COOP/COEP Service Worker failed to control page.");
        return;
      }
      console.log("Reloading page to make use of COOP/COEP Service Worker.");
      storage.setItem("coiReloadedBySelf", "true");
      window.location.reload();
    };

    serviceWorker.addEventListener("controllerchange", reloadOnce);
    serviceWorker
      .register(window.document.currentScript.src)
      .then((registration) => {
        console.log("COOP/COEP Service Worker registered", registration.scope);
        if (registration.active) {
          reloadOnce();
        }
      })
      .catch((error) => {
        console.log("COOP/COEP Service Worker failed to register:", error);
      });
  }
}
