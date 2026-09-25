import init from "./generated/wasm-bindgen/index.js";
import api, { defaultConfig, roundTrip, Mode, Owner, liveOwners, ProbeError, initialize, createPlayer } from "./generated/kithara_config_uniffi_probe";

function check(value: boolean, message: string): asserts value {
  if (!value) throw new Error(message);
}

export async function run() {
  const memory = new WebAssembly.Memory({ initial: 128, maximum: 1024, shared: true });
  await init({ module_or_path: "/generated/wasm-bindgen/index_bg.wasm", memory });
  api.initialize();
  const defaults = defaultConfig();
  check(defaults.revision === 9007199254740993n && defaults.limit === 3, "Rust defaults");
  check(Mode.Window.instanceOf(defaults.nested.mode), "nested enum variant");
  check((defaults.nested.mode as InstanceType<typeof Mode.Window>).inner.frames === 128, "enum payload");
  for (const limit of [undefined, 7]) {
    const value = await roundTrip({ ...defaults, limit });
    check(value.revision === defaults.revision && value.limit === limit, "exact u64/optional");
  }
  let rejected = false;
  try { await roundTrip({ ...defaults, revision: 0n }); }
  catch (error) { rejected = ProbeError.InvalidRevision.instanceOf(error); }
  check(rejected, "typed async error");
  check(liveOwners() === 0, "initial ownership");
  let premature = false;
  try { createPlayer(); } catch (error) { premature = ProbeError.NotInitialized.instanceOf(error); }
  check(premature, "defaults must not initialize host");
  let invalid = false;
  try { await initialize({ ...defaults, revision: 0n }); }
  catch (error) { invalid = ProbeError.InvalidRevision.instanceOf(error); }
  check(invalid, "failed host preparation");
  const initializations = await Promise.allSettled([initialize(defaults), initialize(defaults)]);
  check(initializations[0].status === "fulfilled", "retry after failed init");
  check(initializations[1].status === "rejected" && ProbeError.InitializationInProgress.instanceOf(initializations[1].reason), "concurrent init");
  let repeated = false;
  try { await initialize(defaults); }
  catch (error) { repeated = ProbeError.AlreadyInitialized.instanceOf(error); }
  check(repeated, "repeat init");
  const first = createPlayer(), second = createPlayer();
  check(Owner.instanceOf(first) && Owner.instanceOf(second), "owned player handles");
  first.uniffiDestroy();
  check(second.values().revision === defaults.revision, "independent player lifetime");
  second.uniffiDestroy();
  check(liveOwners() === 0, "players released");
  for (let i = 0; i < 32; i++) {
    const owner = new Owner(defaults);
    check(liveOwners() === 1, "handle created");
    check(owner.values().revision === defaults.revision, "owner readback");
    let notified: bigint | undefined;
    owner.notify({ changed: revision => { notified = revision; } });
    check(notified === defaults.revision, "callback on caller context");
    owner.uniffiDestroy();
    check(liveOwners() === 0, "explicit handle disposal");
    let disposed = false;
    try { owner.values(); } catch { disposed = true; }
    check(disposed, "disposed owner rejects calls");
  }
  check(memory.buffer.byteLength <= 64 * 1024 * 1024, "Wasm memory bound");
  return { revision: defaults.revision, memory: memory.buffer.byteLength, shared: memory };
}
