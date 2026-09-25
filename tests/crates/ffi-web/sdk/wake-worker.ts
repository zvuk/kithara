import init from "./generated/wasm-bindgen/index.js";
import api, { releaseFromWorker } from "./generated/kithara_config_uniffi_probe";

onmessage = async event => {
  try {
    await init({ module_or_path: "/generated/wasm-bindgen/index_bg.wasm", memory: event.data });
    api.initialize();
    postMessage({ released: releaseFromWorker() });
  } catch (error) { postMessage({ error: String(error) }); }
};
