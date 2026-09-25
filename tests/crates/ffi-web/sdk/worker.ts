import { run } from "./contract";

run().then(result => postMessage({ result }), error => postMessage({ error: String(error.stack ?? error) }));
