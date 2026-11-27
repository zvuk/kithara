document.getElementById("output").textContent = "Loading Wasm module...\n";

const worker = new Worker("worker.js", { type: "module" });
worker.onerror = function (e) {
    console.error("Worker error:", e.message, e.filename, e.lineno);
    document.getElementById("output").textContent += "\nWorker error: " + e.message;
};
worker.addEventListener("message", e => {
    if (Array.isArray(e.data) && e.data[0] === "__wbgtest_output_append") {
        document.getElementById("output").textContent += e.data[1];
    }
});
window.__kithara_tests_ready = true;
await window.__kithara_user_activation;
worker.postMessage(__TESTS__);
