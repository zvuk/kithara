import api, { AudioPlayer as UniAudioPlayer, AudioPlayerItem, defaultHostConfig, FfiActionAtItemEnd, FfiCrossfadeCurve, FfiEqFilterKind, FfiError, FfiPlaybackOrder, FfiSizeProbeMethod, initializeHost, tickHost } from "./generated/kithara_ffi";

async function main() {
  const memory = new WebAssembly.Memory({ initial: 128, maximum: 1024, shared: true });
  const { default: init, AudioPlayer } = await import(new URL("./generated/wasm-bindgen/kithara-ffi.js", import.meta.url).href);
  await init({ module_or_path: "/generated/wasm-bindgen/kithara-ffi_bg.wasm", memory });
  api.initialize();

  let prematureTick = false;
  try {
    tickHost();
  } catch (error) {
    prematureTick = FfiError.NotInitialized.instanceOf(error);
  }
  if (!prematureTick) throw new Error("generated host tick accepted an uninitialized host");

  let premature = false;
  try {
    UniAudioPlayer.newWeb();
  } catch (error) {
    premature = FfiError.NotInitialized.instanceOf(error);
  }
  if (!premature) throw new Error("generated player constructor accepted an uninitialized host");

  const defaults = defaultHostConfig();
  if (defaults.sampleRateHint !== 44_100 || Math.abs(defaults.limiter.ceiling - 0.98) > 1e-6 || defaults.limiter.releaseMs !== 50) {
    throw new Error(`wrong Rust host defaults: ${JSON.stringify(defaults)}`);
  }
  let invalid = false;
  try {
    initializeHost({ ...defaults, limiter: { ...defaults.limiter, ceiling: 1.5 } });
  } catch (error) {
    invalid = FfiError.InvalidArgument.instanceOf(error);
  }
  if (!invalid) throw new Error("invalid limiter was accepted");
  invalid = false;
  try {
    initializeHost({ ...defaults, limiter: { ...defaults.limiter, releaseMs: -1 } });
  } catch (error) {
    invalid = FfiError.InvalidArgument.instanceOf(error);
  }
  if (!invalid) throw new Error("invalid release was accepted");

  initializeHost(defaults);
  let repeated = false;
  try {
    initializeHost(defaults);
  } catch (error) {
    repeated = FfiError.AlreadyInitialized.instanceOf(error);
  }
  if (!repeated) throw new Error("repeated host initialization was accepted");
  let invalidQueueSettings = false;
  try {
    UniAudioPlayer.newWebWithQueueSettings({ maxConcurrentLoads: 0 });
  } catch (error) {
    invalidQueueSettings = FfiError.InvalidArgument.instanceOf(error);
  }
  if (!invalidQueueSettings) throw new Error("invalid queue load cap was accepted");
  const initialCrossfade = { duration: 1.5, curve: FfiCrossfadeCurve.Linear, depth: 0.5, position: 0.3 };
  const generatedPlayer = UniAudioPlayer.newWebWithQueueSettings({
    maxConcurrentLoads: 4,
    prefetchDuration: 2,
    crossfadeSettings: initialCrossfade,
  });
  if (JSON.stringify(generatedPlayer.crossfadeSettings()) !== JSON.stringify({ ...initialCrossfade, position: Math.fround(initialCrossfade.position) })) {
    throw new Error("generated queue settings did not reach player readback");
  }
  generatedPlayer.setEqLayout([{ kind: FfiEqFilterKind.Peaking, gainDb: 3, frequency: 1000, qFactor: 0.7 }]);
  if (generatedPlayer.eqBandCount() !== 1 || generatedPlayer.eqGain(0) !== 3) {
    throw new Error("generated player EQ layout did not reach owner readback");
  }
  let oversizedLayout = false;
  try {
    generatedPlayer.setEqLayout(Array(65).fill({ kind: FfiEqFilterKind.Peaking, gainDb: 0, frequency: 1000, qFactor: 0.7 }));
  } catch (error) {
    oversizedLayout = FfiError.InvalidArgument.instanceOf(error);
  }
  if (!oversizedLayout || generatedPlayer.eqBandCount() !== 1) {
    throw new Error("oversized EQ layout changed the player owner");
  }
  const crossfade = { duration: 2.5, curve: FfiCrossfadeCurve.Linear, depth: 0.75, position: 0.4 };
  const pump = setInterval(() => tickHost(), 16);
  const events = new BroadcastChannel("kithara-events");
  const workerApplied = new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("worker did not apply crossfade settings")), 10_000);
    events.onmessage = ({ data }) => {
      if (data.kind !== "CrossfadeSettingsChanged") return;
      clearTimeout(timeout);
      if (data.duration !== crossfade.duration || data.curve !== "Linear" || data.depth !== crossfade.depth || data.position !== Math.fround(crossfade.position)) {
        reject(new Error(`worker applied wrong crossfade settings: ${JSON.stringify(data)}`));
      } else {
        resolve();
      }
    };
  });
  generatedPlayer.setPlayingRate(0);
  if (generatedPlayer.playingRate() !== Math.fround(0.05)) throw new Error("generated playback rate did not clamp to the Rust minimum");
  generatedPlayer.setPlayingRate(0.75);
  if (generatedPlayer.playingRate() !== 0.75) throw new Error("generated playback rate readback is wrong");
  // The crossfade event fences the preceding rate command on the worker queue.
  generatedPlayer.setCrossfadeSettings(crossfade);
  const appliedCrossfade = generatedPlayer.crossfadeSettings();
  if (appliedCrossfade.duration !== crossfade.duration || appliedCrossfade.curve !== crossfade.curve || appliedCrossfade.depth !== crossfade.depth || appliedCrossfade.position !== Math.fround(crossfade.position)) {
    throw new Error(`generated crossfade settings did not reach owner readback: ${JSON.stringify(appliedCrossfade)}`);
  }
  let invalidCrossfade = false;
  try {
    generatedPlayer.setCrossfadeSettings({ ...crossfade, depth: 1.5 });
  } catch (error) {
    invalidCrossfade = FfiError.InvalidArgument.instanceOf(error);
  }
  if (!invalidCrossfade || JSON.stringify(generatedPlayer.crossfadeSettings()) !== JSON.stringify(appliedCrossfade)) {
    throw new Error("invalid crossfade changed the player owner");
  }
  await workerApplied;
  const liveValues = new Promise<void>((resolve, reject) => {
    const seen: string[] = [];
    const timeout = setTimeout(() => reject(new Error(`worker did not retain mute and volume: ${seen.join(",")}`)), 10_000);
    const observe = ({ data }: MessageEvent) => {
      if (data.kind === "MuteChanged") seen.push(`mute:${data.muted}`);
      if (data.kind === "VolumeChanged") seen.push(`volume:${data.volume}`);
      if (seen.length !== 3) return;
      events.removeEventListener("message", observe);
      clearTimeout(timeout);
      if (seen.join(",") === `mute:true,volume:${Math.fround(0.4)},mute:false`) resolve();
      else reject(new Error(`worker retained wrong mute and volume: ${seen.join(",")}`));
    };
    events.addEventListener("message", observe);
  });
  generatedPlayer.setMuted(true);
  generatedPlayer.setVolume(0.4);
  if (!generatedPlayer.isMuted() || generatedPlayer.volume() !== Math.fround(0.4)) {
    throw new Error("generated player lost submitted mute or volume");
  }
  generatedPlayer.setMuted(false);
  await liveValues;
  const queuePolicyApplied = new Promise<void>((resolve, reject) => {
    const seen: string[] = [];
    const timeout = setTimeout(() => reject(new Error(`worker did not apply queue policy: ${seen.join(",")}`)), 10_000);
    const observe = ({ data }: MessageEvent) => {
      if (data.kind === "PlaybackOrderChanged") seen.push(`order:${data.order}`);
      if (data.kind === "ActionAtItemEndChanged") seen.push(`action:${data.action}`);
      if (seen.length !== 2) return;
      events.removeEventListener("message", observe);
      clearTimeout(timeout);
      if (seen.join(",") === "order:Shuffle,action:Pause") resolve();
      else reject(new Error(`worker applied wrong queue policy: ${seen.join(",")}`));
    };
    events.addEventListener("message", observe);
  });
  generatedPlayer.setPlaybackOrder(FfiPlaybackOrder.Shuffle);
  generatedPlayer.setActionAtItemEnd(FfiActionAtItemEnd.Pause);
  if (generatedPlayer.playbackOrder() !== FfiPlaybackOrder.Shuffle || generatedPlayer.actionAtItemEnd() !== FfiActionAtItemEnd.Pause) {
    throw new Error("generated queue policy readback did not retain accepted values");
  }
  let invalidQueuePolicy = false;
  try {
    generatedPlayer.setPlaybackOrder(FfiPlaybackOrder.Unknown);
  } catch (error) {
    invalidQueuePolicy = FfiError.InvalidArgument.instanceOf(error);
  }
  if (!invalidQueuePolicy || generatedPlayer.playbackOrder() !== FfiPlaybackOrder.Shuffle) {
    throw new Error("invalid queue policy changed the generated readback");
  }
  await queuePolicyApplied;
  const url = `${location.origin}/tone.wav`;
  const loadedTrack = new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("generated player did not load the HTTP track")), 10_000);
    events.onmessage = ({ data }) => {
      if (data.kind !== "TrackStatusChanged") return;
      if (data.status === 4) {
        clearTimeout(timeout);
        reject(new Error(`HTTP track failed: ${data.reason}`));
      } else if (data.status === 3) {
        clearTimeout(timeout);
        resolve();
      }
    };
  });
  const itemConfig = { url, abrMode: undefined, audioId: undefined, headers: new Map([["X-Kithara-Config-Probe", "item"]]), uuidI64: undefined, isLiveStream: false, preferredPeakBitrate: 0, preferredPeakBitrateExpensive: 0 };
  const legacyItem = new AudioPlayerItem(itemConfig);
  legacyItem.uniffiDestroy();
  let invalidSource = false;
  try {
    AudioPlayerItem.newWithSourceSettings(itemConfig, { file: { readerEventCapacity: 4097 }, hls: undefined });
  } catch (error) {
    invalidSource = FfiError.InvalidArgument.instanceOf(error);
  }
  if (!invalidSource) throw new Error("unbounded file reader capacity was accepted");
  invalidSource = false;
  try {
    AudioPlayerItem.newWithSourceSettings(
      { ...itemConfig, url: `${location.origin}/live.m3u8` },
      { file: undefined, hls: { sizeProbeMethod: FfiSizeProbeMethod.Unknown } },
    );
  } catch (error) {
    invalidSource = FfiError.InvalidArgument.instanceOf(error);
  }
  if (!invalidSource) throw new Error("unknown HLS size probe method was accepted");
  invalidSource = false;
  try {
    AudioPlayerItem.newWithSourceSettings(
      { ...itemConfig, url: `${location.origin}/live.m3u8` },
      { file: undefined, hls: { acquireAttemptBudget: 256 } },
    );
  } catch (error) {
    invalidSource = FfiError.InvalidArgument.instanceOf(error);
  }
  if (!invalidSource) throw new Error("out-of-range HLS acquire budget was accepted");
  const hlsItem = AudioPlayerItem.newWithSourceSettings(
    { ...itemConfig, url: `${location.origin}/live.m3u8` },
    { file: undefined, hls: { lookAheadBytes: 0n, acquireAttemptBudget: 1, downloadBatchSize: 6, sizeProbeMethod: FfiSizeProbeMethod.RangeGet } },
  ) as AudioPlayerItem;
  hlsItem.uniffiDestroy();
  const item = AudioPlayerItem.newWithSourceSettings(itemConfig, { file: { lookAheadBytes: 0n, readerEventCapacity: 512 }, hls: undefined }) as AudioPlayerItem;
  generatedPlayer.append(item);
  await loadedTrack;
  // firewheel-web-audio resumes its warmed-up AudioContext on a user gesture.
  document.body.click();
  const appliedRate = new Promise<void>((resolve, reject) => {
    const observed: string[] = [];
    const timeout = setTimeout(() => reject(new Error(`audio output did not apply the requested rate; events=${observed.join(",")}`)), 10_000);
    events.onmessage = ({ data }) => {
      observed.push(data.kind);
      if (data.kind !== "RateChanged") return;
      if (data.rate !== 0.75) {
        clearTimeout(timeout);
        reject(new Error(`audio output reported the wrong rate: ${data.rate}`));
      } else {
        clearTimeout(timeout);
        resolve();
      }
    };
  });
  generatedPlayer.play();
  await appliedRate;
  if (generatedPlayer.playingRate() !== 0.75) throw new Error("loaded player lost the requested rate");
  generatedPlayer.pause();
  item.uniffiDestroy();
  clearInterval(pump);
  events.close();
  if (!(generatedPlayer instanceof UniAudioPlayer)) throw new Error("generated player has no owned handle");
  generatedPlayer.uniffiDestroy();
  const player = new AudioPlayer();
  let rejectedBand = false;
  try {
    player.setEqGain(player.eqBandCount(), 3);
  } catch (_) {
    rejectedBand = true;
  }
  if (!rejectedBand || player.eqGain(0) !== 0) throw new Error("invalid EQ band changed readback");
  player.setEqGain(0, 9);
  if (player.eqGain(0) !== 6) throw new Error("EQ readback did not match clamped owner value");
  player.resetEq();
  if (player.eqGain(0) !== 0) throw new Error("EQ reset did not update readback");
  player.free();
  if (memory.buffer.byteLength > 64 * 1024 * 1024) throw new Error("Wasm memory bound exceeded");
  document.body.textContent = `PASS product-host defaults validation lifecycle EQ source settings and applied playback rate; memory=${memory.buffer.byteLength}`;
}

main().catch(error => { document.body.textContent = `FAIL ${error.stack ?? error}`; });
