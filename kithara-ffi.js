import { register_cleanup_handler, wasm_safe_thread_spawn_worker } from './snippets/wasm_safe_thread-6af64f6626d30659/inline1.js';
import { park_notify_at_addr, park_wait_at_addr, park_wait_timeout_at_addr } from './snippets/wasm_safe_thread-6af64f6626d30659/inline2.js';
import * as import1 from "./snippets/wasm_safe_thread-6af64f6626d30659/inline0.js"
import * as import2 from "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js"


/**
 * FFI-facing audio player. A thin facade over the platform-selected
 * `Inner` engine (`NativeInner` on Apple / Android, `WasmInner` on
 * wasm32). Every exported method delegates straight to `inner`; the
 * facade only owns the object identity and (on native) the `Drop`
 * shutdown pulse. The JS control surface lives in
 * `crate::web::surface`.
 */
export class AudioPlayer {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        AudioPlayerFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_audioplayer_free(ptr, 0);
    }
    /**
     * Append a track to the tail of the queue. Returns the allocated
     * track id as an `f64`.
     *
     * # Errors
     * Returns a JS error if the queue rejects the item.
     * @param {string} url
     * @returns {number}
     */
    append(url) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            wasm.audioplayer_append(retptr, this.__wbg_ptr, ptr0, len0);
            var r0 = getDataViewMemory0().getFloat64(retptr + 8 * 0, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
            if (r3) {
                throw takeObject(r2);
            }
            return r0;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {number}
     */
    crossfadeSeconds() {
        const ret = wasm.audioplayer_crossfadeSeconds(this.__wbg_ptr);
        return ret;
    }
    /**
     * Track id (`f64`) of the currently playing item, or `-1.0` if none.
     * @returns {number}
     */
    currentItemId() {
        const ret = wasm.audioplayer_currentItemId(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {number}
     */
    currentTimeMs() {
        const ret = wasm.audioplayer_currentTimeMs(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {number}
     */
    eqBandCount() {
        const ret = wasm.audioplayer_eqBandCount(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @param {number} band
     * @returns {number}
     */
    eqGain(band) {
        const ret = wasm.audioplayer_eqGain(this.__wbg_ptr, band);
        return ret;
    }
    /**
     * Insert a track after the item with `after_id` (or at the head when
     * `after_id` is negative). Returns the new track id.
     *
     * # Errors
     * Returns a JS error if `after_id` is unknown.
     * @param {string} url
     * @param {number} after_id
     * @returns {number}
     */
    insert(url, after_id) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            wasm.audioplayer_insert(retptr, this.__wbg_ptr, ptr0, len0, after_id);
            var r0 = getDataViewMemory0().getFloat64(retptr + 8 * 0, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
            if (r3) {
                throw takeObject(r2);
            }
            return r0;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {boolean}
     */
    isMuted() {
        const ret = wasm.audioplayer_isMuted(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @returns {number}
     */
    itemCount() {
        const ret = wasm.audioplayer_itemCount(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Construct a player. The engine worker and the audio worklet boot
     * lazily on the first command (via the worker bridge), not here: both
     * grow the shared `SharedArrayBuffer` concurrently with the main
     * thread, and wasm-bindgen boxes this handle's `Rc` *after* the
     * constructor body runs. Booting eagerly would box the handle in the
     * middle of that concurrent-grow storm, landing it above the main
     * instance's visible memory bound and trapping every later
     * `__wbg_ptr` deref with "memory access out of bounds". Constructing
     * single-threaded keeps the handle in the main thread's own grown
     * region, always addressable.
     */
    constructor() {
        const ret = wasm.audioplayer_new_js();
        this.__wbg_ptr = ret;
        AudioPlayerFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    pause() {
        wasm.audioplayer_pause(this.__wbg_ptr);
    }
    play() {
        wasm.audioplayer_play(this.__wbg_ptr);
    }
    /**
     * Remove a track by id.
     *
     * # Errors
     * Returns a JS error if the id is not in the queue.
     * @param {number} id
     */
    remove(id) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.audioplayer_remove(retptr, this.__wbg_ptr, id);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    removeAllItems() {
        wasm.audioplayer_removeAllItems(this.__wbg_ptr);
    }
    /**
     * Replace the track at `index`. Returns the new track id.
     *
     * # Errors
     * Returns a JS error if `index` is out of range.
     * @param {number} index
     * @param {string} url
     * @returns {number}
     */
    replaceItem(index, url) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            wasm.audioplayer_replaceItem(retptr, this.__wbg_ptr, index, ptr0, len0);
            var r0 = getDataViewMemory0().getFloat64(retptr + 8 * 0, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
            if (r3) {
                throw takeObject(r2);
            }
            return r0;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Reset every EQ band to 0 dB.
     *
     * # Errors
     * Returns a JS error if the engine rejects the change.
     */
    resetEq() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.audioplayer_resetEq(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @param {number} position_ms
     */
    seek(position_ms) {
        wasm.audioplayer_seek(this.__wbg_ptr, position_ms);
    }
    /**
     * Seek to `position_ms` and invoke the JS callback `obj` with a
     * boolean once the seek command is accepted. Adapts the JS function
     * into the typed `Arc<dyn SeekCallback>` the shared facade `seek`
     * expects.
     *
     * # Errors
     * Returns a JS error if `obj` is not a callable function.
     * @param {number} position_ms
     * @param {any} obj
     */
    seekWithCallback(position_ms, obj) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.audioplayer_seekWithCallback(retptr, this.__wbg_ptr, position_ms, addHeapObject(obj));
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Select (start playing) the track at `index` with an immediate cut.
     *
     * # Errors
     * Returns a JS error if `index` is out of range.
     * @param {number} index
     */
    selectItem(index) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.audioplayer_selectItem(retptr, this.__wbg_ptr, index);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Pin a manual ABR variant by index, or pass a negative index to
     * restore automatic adaptation.
     * @param {number} variant_index
     */
    setAbrMode(variant_index) {
        wasm.audioplayer_setAbrMode(this.__wbg_ptr, variant_index);
    }
    /**
     * @param {number} seconds
     */
    setCrossfadeSeconds(seconds) {
        wasm.audioplayer_setCrossfadeSeconds(this.__wbg_ptr, seconds);
    }
    /**
     * Set the gain (dB) for an EQ band.
     *
     * # Errors
     * Returns a JS error if the engine rejects the change.
     * @param {number} band
     * @param {number} gain_db
     */
    setEqGain(band, gain_db) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.audioplayer_setEqGain(retptr, this.__wbg_ptr, band, gain_db);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Register a JS callback as the per-item observer for the track at
     * `index`. The callback receives marshalled
     * [`FfiItemEvent`](crate::types::FfiItemEvent) objects.
     *
     * # Errors
     * Returns a JS error if `index` is out of range or `obj` is not a
     * callable function.
     * @param {number} index
     * @param {any} obj
     */
    setItemObserver(index, obj) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.audioplayer_setItemObserver(retptr, this.__wbg_ptr, index, addHeapObject(obj));
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @param {boolean} muted
     */
    setMuted(muted) {
        wasm.audioplayer_setMuted(this.__wbg_ptr, muted);
    }
    /**
     * Register a JS callback (`obj`) as the player-level observer. The
     * callback receives one marshalled event object per
     * [`FfiPlayerEvent`](crate::types::FfiPlayerEvent). This is one of
     * the FFI boundary sites: a JS `Function` is adapted into the typed
     * `Arc<dyn PlayerObserver>` the facade expects.
     *
     * # Errors
     * Returns a JS error if `obj` is not a callable function.
     * @param {any} obj
     */
    setObserver(obj) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.audioplayer_setObserver(retptr, this.__wbg_ptr, addHeapObject(obj));
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @param {number} volume
     */
    setVolume(volume) {
        wasm.audioplayer_setVolume(this.__wbg_ptr, volume);
    }
    /**
     * Register a JS DRM key processor (`process_key(key: Uint8Array,
     * salt: string) -> Uint8Array`) and arm a wildcard AES key rule. This
     * is the FFI boundary site that adapts the JS `Function` into the
     * typed `Arc<dyn FfiKeyProcessor>` the facade expects; the worker then
     * routes every segment-key decrypt back to this callback over the
     * cross-thread key bridge.
     *
     * # Errors
     * Returns a JS error if `obj` is not a callable function.
     * @param {any} obj
     */
    setupHlsAes(obj) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.audioplayer_setupHlsAes(retptr, this.__wbg_ptr, addHeapObject(obj));
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Set or clear the player-wide auth token (`X-Auth-Token`). An empty
     * string clears it. Applied to subsequently built tracks.
     * @param {string} auth_token
     */
    setupNetwork(auth_token) {
        const ptr0 = passStringToWasm0(auth_token, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.audioplayer_setupNetwork(this.__wbg_ptr, ptr0, len0);
    }
    stop() {
        wasm.audioplayer_stop(this.__wbg_ptr);
    }
    /**
     * Drive the main-thread pumps once. Call from a
     * `requestAnimationFrame` loop: it polls the worker → main session
     * channel (audio-graph updates) and services pending DRM key requests
     * (invoking the registered JS key callback). Mirrors the legacy
     * `player_tick` for the unified facade.
     */
    tick() {
        wasm.audioplayer_tick(this.__wbg_ptr);
    }
    /**
     * Cap ABR variant selection by per-network peak bitrate (bits/sec).
     * `0.0` lifts the cap for that network.
     * @param {number} wifi_bps
     * @param {number} cellular_bps
     */
    updatePeakBitrate(wifi_bps, cellular_bps) {
        wasm.audioplayer_updatePeakBitrate(this.__wbg_ptr, wifi_bps, cellular_bps);
    }
    /**
     * @returns {number}
     */
    volume() {
        const ret = wasm.audioplayer_volume(this.__wbg_ptr);
        return ret;
    }
}
if (Symbol.dispose) AudioPlayer.prototype[Symbol.dispose] = AudioPlayer.prototype.free;

export class IntoUnderlyingByteSource {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        IntoUnderlyingByteSourceFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_intounderlyingbytesource_free(ptr, 0);
    }
    /**
     * @returns {number}
     */
    get autoAllocateChunkSize() {
        const ret = wasm.intounderlyingbytesource_autoAllocateChunkSize(this.__wbg_ptr);
        return ret >>> 0;
    }
    cancel() {
        const ptr = this.__destroy_into_raw();
        wasm.intounderlyingbytesource_cancel(ptr);
    }
    /**
     * @param {ReadableByteStreamController} controller
     * @returns {Promise<any>}
     */
    pull(controller) {
        const ret = wasm.intounderlyingbytesource_pull(this.__wbg_ptr, addHeapObject(controller));
        return takeObject(ret);
    }
    /**
     * @param {ReadableByteStreamController} controller
     */
    start(controller) {
        wasm.intounderlyingbytesource_start(this.__wbg_ptr, addHeapObject(controller));
    }
    /**
     * @returns {ReadableStreamType}
     */
    get type() {
        const ret = wasm.intounderlyingbytesource_type(this.__wbg_ptr);
        return __wbindgen_enum_ReadableStreamType[ret];
    }
}
if (Symbol.dispose) IntoUnderlyingByteSource.prototype[Symbol.dispose] = IntoUnderlyingByteSource.prototype.free;

export class IntoUnderlyingSink {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        IntoUnderlyingSinkFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_intounderlyingsink_free(ptr, 0);
    }
    /**
     * @param {any} reason
     * @returns {Promise<any>}
     */
    abort(reason) {
        const ptr = this.__destroy_into_raw();
        const ret = wasm.intounderlyingsink_abort(ptr, addHeapObject(reason));
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    close() {
        const ptr = this.__destroy_into_raw();
        const ret = wasm.intounderlyingsink_close(ptr);
        return takeObject(ret);
    }
    /**
     * @param {any} chunk
     * @returns {Promise<any>}
     */
    write(chunk) {
        const ret = wasm.intounderlyingsink_write(this.__wbg_ptr, addHeapObject(chunk));
        return takeObject(ret);
    }
}
if (Symbol.dispose) IntoUnderlyingSink.prototype[Symbol.dispose] = IntoUnderlyingSink.prototype.free;

export class IntoUnderlyingSource {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        IntoUnderlyingSourceFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_intounderlyingsource_free(ptr, 0);
    }
    cancel() {
        const ptr = this.__destroy_into_raw();
        wasm.intounderlyingsource_cancel(ptr);
    }
    /**
     * @param {ReadableStreamDefaultController} controller
     * @returns {Promise<any>}
     */
    pull(controller) {
        const ret = wasm.intounderlyingsource_pull(this.__wbg_ptr, addHeapObject(controller));
        return takeObject(ret);
    }
}
if (Symbol.dispose) IntoUnderlyingSource.prototype[Symbol.dispose] = IntoUnderlyingSource.prototype.free;

export class ProcessorHost {
    static __wrap(ptr) {
        const obj = Object.create(ProcessorHost.prototype);
        obj.__wbg_ptr = ptr;
        ProcessorHostFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        ProcessorHostFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_processorhost_free(ptr, 0);
    }
    /**
     * Pack the object to send through the web audio worklet constructor
     * @returns {number}
     */
    pack() {
        const ptr = this.__destroy_into_raw();
        const ret = wasm.processorhost_pack(ptr);
        return ret >>> 0;
    }
    /**
     * @param {Array<any>} inputs
     * @param {Array<any>} outputs
     * @param {number} current_time
     * @returns {boolean}
     */
    process(inputs, outputs, current_time) {
        const ret = wasm.processorhost_process(this.__wbg_ptr, addHeapObject(inputs), addHeapObject(outputs), current_time);
        return ret !== 0;
    }
    /**
     * Unpack the object from the worklet constructor
     * # Safety
     * This should only be called within the worklet constructor from a known
     * good pointer
     * @param {number} ptr
     * @returns {ProcessorHost}
     */
    static unpack(ptr) {
        const ret = wasm.processorhost_unpack(ptr);
        return ProcessorHost.__wrap(ret);
    }
}
if (Symbol.dispose) ProcessorHost.prototype[Symbol.dispose] = ProcessorHost.prototype.free;

/**
 * Build revision string: `"version git_hash build_timestamp"`.
 * @returns {string}
 */
export function build_info() {
    let deferred1_0;
    let deferred1_1;
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.build_info(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        deferred1_0 = r0;
        deferred1_1 = r1;
        return getStringFromWasm0(r0, r1);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
        wasm.__wbindgen_export4(deferred1_0, deferred1_1, 1);
    }
}

export function setup() {
    wasm.setup();
}

/**
 * Entry point invoked by JavaScript in a worker.
 * @param {number} ptr
 */
export function task_worker_entry_point(ptr) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.task_worker_entry_point(retptr, ptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        if (r1) {
            throw takeObject(r0);
        }
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {any} work
 */
export function wasm_safe_thread_entry_point(work) {
    wasm.wasm_safe_thread_entry_point(addHeapObject(work));
}

/**
 * Returns the number of pending async tasks.
 *
 * This is exported for use by the worker's JavaScript code to wait for
 * all tasks to complete before closing.
 * @returns {number}
 */
export function wasm_safe_thread_pending_tasks() {
    const ret = wasm.wasm_safe_thread_pending_tasks();
    return ret >>> 0;
}
function __wbg_get_imports(memory) {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_boolean_get_c3dd5c39f1b5a12b: function(arg0) {
            const v = getObject(arg0);
            const ret = typeof(v) === 'boolean' ? v : undefined;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg___wbindgen_debug_string_07cb72cfcc952e2b: function(arg0, arg1) {
            const ret = debugString(getObject(arg1));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_2f0fd7ceb86e64c5: function(arg0) {
            const ret = typeof(getObject(arg0)) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_undefined_244a92c34d3b6ec0: function(arg0) {
            const ret = getObject(arg0) === undefined;
            return ret;
        },
        __wbg___wbindgen_jsval_eq_403eaa3610500a25: function(arg0, arg1) {
            const ret = getObject(arg0) === getObject(arg1);
            return ret;
        },
        __wbg___wbindgen_memory_c2356dd1a089dfbd: function() {
            const ret = wasm.memory;
            return addHeapObject(ret);
        },
        __wbg___wbindgen_module_df704393dfd1853c: function() {
            const ret = wasmModule;
            return addHeapObject(ret);
        },
        __wbg___wbindgen_number_get_dd6d69a6079f26f1: function(arg0, arg1) {
            const obj = getObject(arg1);
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_rethrow_8e609956a7b9f4fb: function(arg0) {
            throw takeObject(arg0);
        },
        __wbg___wbindgen_string_get_965592073e5d848c: function(arg0, arg1) {
            const obj = getObject(arg1);
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_9c75d47bf9e7731e: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_158e43e869788cdc: function(arg0) {
            getObject(arg0)._wbg_cb_unref();
        },
        __wbg_abort_43913e33ecb83d0d: function(arg0, arg1) {
            getObject(arg0).abort(getObject(arg1));
        },
        __wbg_abort_87eb7f23cf4b73d1: function(arg0) {
            getObject(arg0).abort();
        },
        __wbg_addEventListener_a95e75babfc4f5a3: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            getObject(arg0).addEventListener(getStringFromWasm0(arg1, arg2), getObject(arg3));
        }, arguments); },
        __wbg_addModule_38f20f116415be5b: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).addModule(getStringFromWasm0(arg1, arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_append_8df396311184f750: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            getObject(arg0).append(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_async_1ee5bed8fb1cc6ba: function(arg0) {
            const ret = getObject(arg0).async;
            return ret;
        },
        __wbg_audioWorklet_c93047ef46baf23a: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).audioWorklet;
            return addHeapObject(ret);
        }, arguments); },
        __wbg_body_6929614c20dfa7b0: function(arg0) {
            const ret = getObject(arg0).body;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_buffer_500ec46e6722f492: function(arg0) {
            const ret = getObject(arg0).buffer;
            return addHeapObject(ret);
        },
        __wbg_buffer_9ee17426fe5a5d65: function(arg0) {
            const ret = getObject(arg0).buffer;
            return addHeapObject(ret);
        },
        __wbg_byobRequest_178b64c09a0bee03: function(arg0) {
            const ret = getObject(arg0).byobRequest;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_byteLength_1f57c71e64ee0180: function(arg0) {
            const ret = getObject(arg0).byteLength;
            return ret;
        },
        __wbg_byteOffset_648d0af273024f3d: function(arg0) {
            const ret = getObject(arg0).byteOffset;
            return ret;
        },
        __wbg_call_a41d6421b30a32c5: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).call(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_call_a6d9545202d34317: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = getObject(arg0).call(getObject(arg1), getObject(arg2), getObject(arg3));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_cancel_f97a3ee5a8b30eef: function(arg0) {
            const ret = getObject(arg0).cancel();
            return addHeapObject(ret);
        },
        __wbg_catch_f939343cb181958c: function(arg0, arg1) {
            const ret = getObject(arg0).catch(getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_clearTimeout_333bba87532ab9d3: function(arg0) {
            const ret = clearTimeout(takeObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_close_1dd84b3ac8a28727: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).close();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_close_63e009c5a75f5597: function() { return handleError(function (arg0) {
            getObject(arg0).close();
        }, arguments); },
        __wbg_close_de471367367aa5cb: function() { return handleError(function (arg0) {
            getObject(arg0).close();
        }, arguments); },
        __wbg_close_f3746c13c63c3789: function(arg0) {
            getObject(arg0).close();
        },
        __wbg_connect_b0c6d44e9984ca8e: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg0).connect(getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_createObjectURL_ff4de9deb3f8d0a6: function() { return handleError(function (arg0, arg1) {
            const ret = URL.createObjectURL(getObject(arg1));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_data_0ba4ecacc6f43a18: function(arg0) {
            const ret = getObject(arg0).data;
            return addHeapObject(ret);
        },
        __wbg_data_4a14fad4c5f216c4: function(arg0) {
            const ret = getObject(arg0).data;
            return addHeapObject(ret);
        },
        __wbg_destination_a7fb84721246ff2f: function(arg0) {
            const ret = getObject(arg0).destination;
            return addHeapObject(ret);
        },
        __wbg_disconnect_b9d5c901176aac80: function() { return handleError(function (arg0) {
            getObject(arg0).disconnect();
        }, arguments); },
        __wbg_document_69bb6a2f7927d532: function(arg0) {
            const ret = getObject(arg0).document;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_done_b1afd6201ac045e0: function(arg0) {
            const ret = getObject(arg0).done;
            return ret;
        },
        __wbg_enqueue_6c7cd543c0f3828e: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).enqueue(getObject(arg1));
        }, arguments); },
        __wbg_entries_83f42485034accab: function(arg0) {
            const ret = getObject(arg0).entries();
            return addHeapObject(ret);
        },
        __wbg_error_620e7bbed54be6fa: function(arg0, arg1) {
            console.error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_error_7aabf7ad5c35cfbb: function(arg0, arg1) {
            console.error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_error_a6fa202b58aa1cd3: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_export4(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_eval_b3ce086b62c3ca2e: function() { return handleError(function (arg0, arg1) {
            const ret = eval(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_fetch_074561c3e313c86f: function(arg0) {
            const ret = fetch(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_fetch_1a030943aa8e0c38: function(arg0, arg1) {
            const ret = getObject(arg0).fetch(getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_getReader_9facd4f899beac89: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).getReader();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_getUserMedia_821dca2f40146312: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg0).getUserMedia(getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_get_41476db20fef99a8: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_get_652f640b3b0b6e3e: function(arg0, arg1) {
            const ret = getObject(arg0)[arg1 >>> 0];
            return addHeapObject(ret);
        },
        __wbg_get_done_2088079830fb242e: function(arg0) {
            const ret = getObject(arg0).done;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg_get_value_52f4b39f58a812ed: function(arg0) {
            const ret = getObject(arg0).value;
            return addHeapObject(ret);
        },
        __wbg_has_3a6f31f647e0ba22: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.has(getObject(arg0), getObject(arg1));
            return ret;
        }, arguments); },
        __wbg_headers_de17f740bce997ae: function(arg0) {
            const ret = getObject(arg0).headers;
            return addHeapObject(ret);
        },
        __wbg_instanceof_DedicatedWorkerGlobalScope_32441a4d2ca5f2c4: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof DedicatedWorkerGlobalScope;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_ErrorEvent_bca0ce3b26671585: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof ErrorEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Float32Array_a790b72148c24a2a: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Float32Array;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_MediaStream_c6f1caa3f5a9dc73: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof MediaStream;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_MessageEvent_ec94d51dda7932d8: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof MessageEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Response_370b83aa6c17e88a: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Response;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Uint8Array_57d77acd50e4c44d: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Uint8Array;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_4153c1818a1c0c0b: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_isArray_c6c6ef8308995bcf: function(arg0) {
            const ret = Array.isArray(getObject(arg0));
            return ret;
        },
        __wbg_length_5693120f2a64a00d: function(arg0) {
            const ret = getObject(arg0).length;
            return ret;
        },
        __wbg_length_ba3c032602efe310: function(arg0) {
            const ret = getObject(arg0).length;
            return ret;
        },
        __wbg_log_0c201ade58bb55e1: function(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.log(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3), getStringFromWasm0(arg4, arg5), getStringFromWasm0(arg6, arg7));
            } finally {
                wasm.__wbindgen_export4(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_log_72d22df918dcc232: function(arg0) {
            console.log(getObject(arg0));
        },
        __wbg_log_bf735d3e0877667a: function(arg0, arg1) {
            console.log(getStringFromWasm0(arg0, arg1));
        },
        __wbg_log_ce2c4456b290c5e7: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.log(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_export4(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_mark_b4d943f3bc2d2404: function(arg0, arg1) {
            performance.mark(getStringFromWasm0(arg0, arg1));
        },
        __wbg_measure_84362959e621a2c1: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            let deferred0_0;
            let deferred0_1;
            let deferred1_0;
            let deferred1_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                deferred1_0 = arg2;
                deferred1_1 = arg3;
                performance.measure(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
            } finally {
                wasm.__wbindgen_export4(deferred0_0, deferred0_1, 1);
                wasm.__wbindgen_export4(deferred1_0, deferred1_1, 1);
            }
        }, arguments); },
        __wbg_mediaDevices_167ec4ea851d9ed9: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).mediaDevices;
            return addHeapObject(ret);
        }, arguments); },
        __wbg_message_75a790b5f7b409d0: function(arg0, arg1) {
            const ret = getObject(arg1).message;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_navigator_f3468c6dc9006b7c: function(arg0) {
            const ret = getObject(arg0).navigator;
            return addHeapObject(ret);
        },
        __wbg_new_18865c63fa645c6f: function() { return handleError(function () {
            const ret = new Headers();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return addHeapObject(ret);
        },
        __wbg_new_2fad8ca02fd00684: function() {
            const ret = new Object();
            return addHeapObject(ret);
        },
        __wbg_new_3baa8d9866155c79: function() {
            const ret = new Array();
            return addHeapObject(ret);
        },
        __wbg_new_51ff470dc2f61e27: function() { return handleError(function () {
            const ret = new AbortController();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_800ed232e27e56db: function() { return handleError(function (arg0, arg1) {
            const ret = new MediaStreamAudioSourceNode(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_8454eee672b2ba6e: function(arg0) {
            const ret = new Uint8Array(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_new_a6b46eaf9085fbeb: function() { return handleError(function () {
            const ret = new lAudioContext();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_b92364ac5202a6de: function(arg0) {
            const ret = new Int32Array(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_new_c46e956c93e006da: function() { return handleError(function (arg0, arg1) {
            const ret = new BroadcastChannel(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_c9ea13ea803a692e: function(arg0, arg1) {
            const ret = new Error(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_eb8acd9352be84ba: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return __wasm_bindgen_func_elem_16735(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return addHeapObject(ret);
            } finally {
                state0.a = 0;
            }
        },
        __wbg_new_from_slice_5a173c243af2e823: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_typed_1137602701dc87d4: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return __wasm_bindgen_func_elem_16735(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return addHeapObject(ret);
            } finally {
                state0.a = 0;
            }
        },
        __wbg_new_with_blob_sequence_and_options_237bab00175bd109: function() { return handleError(function (arg0, arg1) {
            const ret = new Blob(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_with_byte_offset_and_length_643e5e9e2fb6b1ad: function(arg0, arg1, arg2) {
            const ret = new Uint8Array(getObject(arg0), arg1 >>> 0, arg2 >>> 0);
            return addHeapObject(ret);
        },
        __wbg_new_with_context_options_35b54106e5e7abc6: function() { return handleError(function (arg0) {
            const ret = new lAudioContext(getObject(arg0));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_with_length_9011f5da794bf5d9: function(arg0) {
            const ret = new Uint8Array(arg0 >>> 0);
            return addHeapObject(ret);
        },
        __wbg_new_with_options_a99de022c218da8c: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = new Worker(getStringFromWasm0(arg0, arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_with_options_bb56485fdfcd1251: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = new AudioWorkletNode(getObject(arg0), getStringFromWasm0(arg1, arg2), getObject(arg3));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_with_str_and_init_da311e12114f4d1e: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = new Request(getStringFromWasm0(arg0, arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_with_str_sequence_and_options_d582f60b3b1caf49: function() { return handleError(function (arg0, arg1) {
            const ret = new Blob(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_worker_587767f5b778f6ce: function(arg0, arg1) {
            const ret = new Worker(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_next_aacee310bcfe6461: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).next();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_now_333d66783e282362: function() {
            const ret = Date.now();
            return ret;
        },
        __wbg_now_e7c6795a7f81e10f: function(arg0) {
            const ret = getObject(arg0).now();
            return ret;
        },
        __wbg_of_24ccb247709bafd2: function(arg0, arg1, arg2) {
            const ret = Array.of(getObject(arg0), getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        },
        __wbg_of_96154841226db59c: function(arg0, arg1) {
            const ret = Array.of(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_of_cc555051dc9558d3: function(arg0) {
            const ret = Array.of(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_park_notify_at_addr_078552077bb71254: function(arg0, arg1) {
            park_notify_at_addr(getObject(arg0), arg1 >>> 0);
        },
        __wbg_park_wait_at_addr_f11d67a9f8b3c8f9: function(arg0, arg1) {
            const ret = park_wait_at_addr(getObject(arg0), arg1 >>> 0);
            return addHeapObject(ret);
        },
        __wbg_park_wait_timeout_at_addr_c086eefff696f392: function(arg0, arg1, arg2) {
            const ret = park_wait_timeout_at_addr(getObject(arg0), arg1 >>> 0, arg2);
            return addHeapObject(ret);
        },
        __wbg_performance_3fcf6e32a7e1ed0a: function(arg0) {
            const ret = getObject(arg0).performance;
            return addHeapObject(ret);
        },
        __wbg_postMessage_06791ee3f1b34f05: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).postMessage(getObject(arg1));
        }, arguments); },
        __wbg_postMessage_b8899b5b0ca9ad5f: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).postMessage(getObject(arg1));
        }, arguments); },
        __wbg_postMessage_d337216cda0e6002: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).postMessage(getObject(arg1));
        }, arguments); },
        __wbg_postMessage_ead2ef5ee8c7a94e: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).postMessage(getObject(arg1));
        }, arguments); },
        __wbg_prototypesetcall_f2b501ba26592df2: function(arg0, arg1, arg2) {
            Float32Array.prototype.set.call(getArrayF32FromWasm0(arg0, arg1), getObject(arg2));
        },
        __wbg_prototypesetcall_fd4050e806e1d519: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), getObject(arg2));
        },
        __wbg_push_60a5366c0bb22a7d: function(arg0, arg1) {
            const ret = getObject(arg0).push(getObject(arg1));
            return ret;
        },
        __wbg_queueMicrotask_40ac6ffc2848ba77: function(arg0) {
            queueMicrotask(getObject(arg0));
        },
        __wbg_queueMicrotask_74d092439f6494c1: function(arg0) {
            const ret = getObject(arg0).queueMicrotask;
            return addHeapObject(ret);
        },
        __wbg_read_ac2e4325f1799cbe: function(arg0) {
            const ret = getObject(arg0).read();
            return addHeapObject(ret);
        },
        __wbg_register_cleanup_handler_e9b3ba4d7db15312: function(arg0) {
            register_cleanup_handler(getObject(arg0));
        },
        __wbg_releaseLock_9e0ebc0b5270a358: function(arg0) {
            getObject(arg0).releaseLock();
        },
        __wbg_removeEventListener_2ce4c0697d2b692c: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            getObject(arg0).removeEventListener(getStringFromWasm0(arg1, arg2), getObject(arg3));
        }, arguments); },
        __wbg_resolve_9feb5d906ca62419: function(arg0) {
            const ret = Promise.resolve(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_respond_e7e53102735b2ae2: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).respond(arg1 >>> 0);
        }, arguments); },
        __wbg_resume_60c7fdf589dd7208: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).resume();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_sampleRate_b7f221c5b3d93248: function(arg0) {
            const ret = getObject(arg0).sampleRate;
            return ret;
        },
        __wbg_setTimeout_2a1fbbfbc00febf2: function(arg0, arg1) {
            globalThis.setTimeout(getObject(arg0), arg1);
        },
        __wbg_setTimeout_3a808dd861dd3c12: function(arg0, arg1) {
            const ret = setTimeout(getObject(arg0), arg1);
            return addHeapObject(ret);
        },
        __wbg_set_15b3678c712ded6b: function(arg0, arg1, arg2) {
            getObject(arg0).set(getArrayF32FromWasm0(arg1, arg2));
        },
        __wbg_set_5337f8ac82364a3f: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(getObject(arg0), getObject(arg1), getObject(arg2));
            return ret;
        }, arguments); },
        __wbg_set_audio_40010dc565f39cb8: function(arg0, arg1) {
            getObject(arg0).audio = getObject(arg1);
        },
        __wbg_set_b0d9dc239ecdb765: function(arg0, arg1, arg2) {
            getObject(arg0).set(getArrayU8FromWasm0(arg1, arg2));
        },
        __wbg_set_body_aaff4f5f9991f342: function(arg0, arg1) {
            getObject(arg0).body = getObject(arg1);
        },
        __wbg_set_cache_d1f2b7b4dfa39317: function(arg0, arg1) {
            getObject(arg0).cache = __wbindgen_enum_RequestCache[arg1];
        },
        __wbg_set_channel_count_3bf6e7ee06afa77a: function(arg0, arg1) {
            getObject(arg0).channelCount = arg1 >>> 0;
        },
        __wbg_set_credentials_f31e4d30b974ce14: function(arg0, arg1) {
            getObject(arg0).credentials = __wbindgen_enum_RequestCredentials[arg1];
        },
        __wbg_set_headers_ae96049ea40e9eef: function(arg0, arg1) {
            getObject(arg0).headers = getObject(arg1);
        },
        __wbg_set_media_stream_dcef6dd5eba179ab: function(arg0, arg1) {
            getObject(arg0).mediaStream = getObject(arg1);
        },
        __wbg_set_method_0eea8a5597775fa1: function(arg0, arg1, arg2) {
            getObject(arg0).method = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_mode_9fe47bff60a1580d: function(arg0, arg1) {
            getObject(arg0).mode = __wbindgen_enum_RequestMode[arg1];
        },
        __wbg_set_number_of_inputs_288befc2b0c5ff4c: function(arg0, arg1) {
            getObject(arg0).numberOfInputs = arg1 >>> 0;
        },
        __wbg_set_number_of_outputs_eadd94522154caf9: function(arg0, arg1) {
            getObject(arg0).numberOfOutputs = arg1 >>> 0;
        },
        __wbg_set_onerror_09df71d2285b58a1: function(arg0, arg1) {
            getObject(arg0).onerror = getObject(arg1);
        },
        __wbg_set_onmessage_146e69bce551b1b6: function(arg0, arg1) {
            getObject(arg0).onmessage = getObject(arg1);
        },
        __wbg_set_onmessage_5c487e2bc6858454: function(arg0, arg1) {
            getObject(arg0).onmessage = getObject(arg1);
        },
        __wbg_set_onmessage_ad6719fac02b2b01: function(arg0, arg1) {
            getObject(arg0).onmessage = getObject(arg1);
        },
        __wbg_set_onprocessorerror_2c754c40a6c37aa3: function(arg0, arg1) {
            getObject(arg0).onprocessorerror = getObject(arg1);
        },
        __wbg_set_output_channel_count_6223928b34197899: function(arg0, arg1) {
            getObject(arg0).outputChannelCount = getObject(arg1);
        },
        __wbg_set_processor_options_8ab11283088bff82: function(arg0, arg1) {
            getObject(arg0).processorOptions = getObject(arg1);
        },
        __wbg_set_sample_rate_2684ef2c0111ef24: function(arg0, arg1) {
            getObject(arg0).sampleRate = arg1;
        },
        __wbg_set_signal_8c5cf4c3b27bd8a8: function(arg0, arg1) {
            getObject(arg0).signal = getObject(arg1);
        },
        __wbg_set_type_86c28c059175fa05: function(arg0, arg1) {
            getObject(arg0).type = __wbindgen_enum_WorkerType[arg1];
        },
        __wbg_set_type_9cc8db71b8673ad7: function(arg0, arg1, arg2) {
            getObject(arg0).type = getStringFromWasm0(arg1, arg2);
        },
        __wbg_signal_4643ce883b92b553: function(arg0) {
            const ret = getObject(arg0).signal;
            return addHeapObject(ret);
        },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = getObject(arg1).stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_state_888c5182164828c7: function(arg0) {
            const ret = getObject(arg0).state;
            return (__wbindgen_enum_AudioContextState.indexOf(ret) + 1 || 4) - 1;
        },
        __wbg_static_accessor_GLOBAL_THIS_1c7f1bd6c6941fdb: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_GLOBAL_e039bc914f83e74e: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_IMPORT_META_a4b382c4359199c3: function() {
            const ret = import.meta;
            return addHeapObject(ret);
        },
        __wbg_static_accessor_SELF_8bf8c48c28420ad5: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_WINDOW_6aeee9b51652ee0f: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_status_157e67ab07d01f8a: function(arg0) {
            const ret = getObject(arg0).status;
            return ret;
        },
        __wbg_suspend_52df7cf9a7b9e0d6: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).suspend();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_terminate_0fb5d4cd8218988f: function(arg0) {
            getObject(arg0).terminate();
        },
        __wbg_text_de416916b5c06490: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).text();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_then_20a157d939b514f5: function(arg0, arg1) {
            const ret = getObject(arg0).then(getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_then_4d0dc09d0334f8a0: function(arg0, arg1) {
            const ret = getObject(arg0).then(getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_then_5ef9b762bc91555c: function(arg0, arg1, arg2) {
            const ret = getObject(arg0).then(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        },
        __wbg_timeOrigin_f3d5cb4f4a06c2b7: function(arg0) {
            const ret = getObject(arg0).timeOrigin;
            return ret;
        },
        __wbg_url_89d3827eb946d99a: function(arg0) {
            const ret = getObject(arg0).url;
            return addHeapObject(ret);
        },
        __wbg_url_a0e994e7d0317efc: function(arg0, arg1) {
            const ret = getObject(arg1).url;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_value_9a45af0e26b1f87c: function(arg0) {
            const ret = getObject(arg0).value;
            return addHeapObject(ret);
        },
        __wbg_value_f852716acdeb3e82: function(arg0) {
            const ret = getObject(arg0).value;
            return addHeapObject(ret);
        },
        __wbg_view_16bd97d49793e1a9: function(arg0) {
            const ret = getObject(arg0).view;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_waitAsync_46b9c16917402b6b: function(arg0, arg1, arg2) {
            const ret = Atomics.waitAsync(getObject(arg0), arg1 >>> 0, arg2);
            return addHeapObject(ret);
        },
        __wbg_waitAsync_5c459d2d0295c202: function() {
            const ret = Atomics.waitAsync;
            return addHeapObject(ret);
        },
        __wbg_warn_1f9b94806da61fbb: function(arg0) {
            console.warn(getObject(arg0));
        },
        __wbg_wasm_safe_thread_spawn_worker_691b5a5ca7ddcb3d: function(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7) {
            const ret = wasm_safe_thread_spawn_worker(takeObject(arg0), takeObject(arg1), takeObject(arg2), getStringFromWasm0(arg3, arg4), getStringFromWasm0(arg5, arg6), arg7 >>> 0);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 2371, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_14387);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 2501, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_15238);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 2572, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_16732);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 2574, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_16734);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 628, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_3734);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000006: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("ErrorEvent")], shim_idx: 627, ret: Unit, inner_ret: Some(Unit) }, mutable: false }) -> Externref`.
            const ret = makeClosure(arg0, arg1, __wasm_bindgen_func_elem_3733);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000007: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("Event")], shim_idx: 2523, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_15387);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000008: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("Event")], shim_idx: 628, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_3734_7);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000009: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 3, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_523);
            return addHeapObject(ret);
        },
        __wbindgen_cast_000000000000000a: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [U32], shim_idx: 2546, ret: Unit, inner_ret: Some(Unit) }, mutable: false }) -> Externref`.
            const ret = makeClosure(arg0, arg1, __wasm_bindgen_func_elem_16555);
            return addHeapObject(ret);
        },
        __wbindgen_cast_000000000000000b: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 2334, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_14168);
            return addHeapObject(ret);
        },
        __wbindgen_cast_000000000000000c: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return addHeapObject(ret);
        },
        __wbindgen_cast_000000000000000d: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return addHeapObject(ret);
        },
        __wbindgen_link_05d8570477813ff4: function(arg0) {
            const val = `onmessage = function (ev) {
                let [ia, index, value] = ev.data;
                ia = new Int32Array(ia.buffer);
                let result = Atomics.wait(ia, index, value);
                postMessage(result);
            };
            `;
            const ret = typeof URL.createObjectURL === 'undefined' ? "data:application/javascript," + encodeURIComponent(val) : URL.createObjectURL(new Blob([val], { type: "text/javascript" }));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbindgen_object_clone_ref: function(arg0) {
            const ret = getObject(arg0);
            return addHeapObject(ret);
        },
        __wbindgen_object_drop_ref: function(arg0) {
            takeObject(arg0);
        },
        memory: memory || new WebAssembly.Memory({initial:4097,maximum:24576,shared:true}),
    };
    return {
        __proto__: null,
        "./kithara-ffi_bg.js": import0,
        "./snippets/wasm_safe_thread-6af64f6626d30659/inline0.js": import1,
        "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js": import2,
    };
}

const lAudioContext = (typeof AudioContext !== 'undefined' ? AudioContext : (typeof webkitAudioContext !== 'undefined' ? webkitAudioContext : undefined));
function __wasm_bindgen_func_elem_14168(arg0, arg1) {
    wasm.__wasm_bindgen_func_elem_14168(arg0, arg1);
}

function __wasm_bindgen_func_elem_14387(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_14387(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_15238(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_15238(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_16734(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_16734(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_3734(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_3734(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_3733(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_3733(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_15387(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_15387(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_3734_7(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_3734_7(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_523(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_523(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_16555(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_16555(arg0, arg1, arg2);
}

function __wasm_bindgen_func_elem_16732(arg0, arg1, arg2) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.__wasm_bindgen_func_elem_16732(retptr, arg0, arg1, addHeapObject(arg2));
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        if (r1) {
            throw takeObject(r0);
        }
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

function __wasm_bindgen_func_elem_16735(arg0, arg1, arg2, arg3) {
    wasm.__wasm_bindgen_func_elem_16735(arg0, arg1, addHeapObject(arg2), addHeapObject(arg3));
}


const __wbindgen_enum_AudioContextState = ["suspended", "running", "closed"];


const __wbindgen_enum_ReadableStreamType = ["bytes"];


const __wbindgen_enum_RequestCache = ["default", "no-store", "reload", "no-cache", "force-cache", "only-if-cached"];


const __wbindgen_enum_RequestCredentials = ["omit", "same-origin", "include"];


const __wbindgen_enum_RequestMode = ["same-origin", "no-cors", "cors", "navigate"];


const __wbindgen_enum_WorkerType = ["classic", "module"];
const AudioPlayerFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_audioplayer_free(ptr, 1));
const IntoUnderlyingByteSourceFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_intounderlyingbytesource_free(ptr, 1));
const IntoUnderlyingSinkFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_intounderlyingsink_free(ptr, 1));
const IntoUnderlyingSourceFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_intounderlyingsource_free(ptr, 1));
const ProcessorHostFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_processorhost_free(ptr, 1));

function addHeapObject(obj) {
    if (heap_next === heap.length) heap.push(heap.length + 1);
    const idx = heap_next;
    heap_next = heap[idx];

    heap[idx] = obj;
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => wasm.__wbindgen_export5(state.a, state.b));

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function dropObject(idx) {
    if (idx < 1028) return;
    heap[idx] = heap_next;
    heap_next = idx;
}

function getArrayF32FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getFloat32ArrayMemory0().subarray(ptr / 4, ptr / 4 + len);
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer !== wasm.memory.buffer) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

let cachedFloat32ArrayMemory0 = null;
function getFloat32ArrayMemory0() {
    if (cachedFloat32ArrayMemory0 === null || cachedFloat32ArrayMemory0.buffer !== wasm.memory.buffer) {
        cachedFloat32ArrayMemory0 = new Float32Array(wasm.memory.buffer);
    }
    return cachedFloat32ArrayMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.buffer !== wasm.memory.buffer) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function getObject(idx) { return heap[idx]; }

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        wasm.__wbindgen_export3(addHeapObject(e));
    }
}

let heap = new Array(1024).fill(undefined);
heap.push(undefined, null, true, false);

let heap_next = heap.length;

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeClosure(arg0, arg1, f) {
    const state = { a: arg0, b: arg1, cnt: 1 };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        try {
            return f(state.a, state.b, ...args);
        } finally {
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            wasm.__wbindgen_export5(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function makeMutClosure(arg0, arg1, f) {
    const state = { a: arg0, b: arg1, cnt: 1 };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            wasm.__wbindgen_export5(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeObject(idx) {
    const ret = getObject(idx);
    dropObject(idx);
    return ret;
}

if(typeof TextDecoder==="undefined"){globalThis.TextDecoder=class{constructor(){}decode(b){if(!b||!b.length)return"";let r="";for(let i=0;i<b.length;i++)r+=String.fromCharCode(b[i]);return r}};globalThis.TextEncoder=class{constructor(){}encode(s){const a=new Uint8Array(s.length);for(let i=0;i<s.length;i++)a[i]=s.charCodeAt(i);return a}encodeInto(s,d){const e=this.encode(s);d.set(e);return{read:s.length,written:e.length}}}}
let cachedTextDecoder = (typeof TextDecoder !== 'undefined' ? new TextDecoder('utf-8', { ignoreBOM: true, fatal: true }) : undefined);
if (cachedTextDecoder) cachedTextDecoder.decode();

const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().slice(ptr, ptr + len));
}

const cachedTextEncoder = (typeof TextEncoder !== 'undefined' ? new TextEncoder() : undefined);

if (cachedTextEncoder) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module, thread_stack_size) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedFloat32ArrayMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    if (typeof thread_stack_size !== 'undefined' && (typeof thread_stack_size !== 'number' || thread_stack_size === 0 || thread_stack_size % 65536 !== 0)) {
        throw new Error('invalid stack size');
    }

    wasm.__wbindgen_start(thread_stack_size);
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module, memory) {
    if (wasm !== undefined) return wasm;

    let thread_stack_size
    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module, memory, thread_stack_size} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports(memory);
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module, thread_stack_size);
}

async function __wbg_init(module_or_path, memory) {
    if (wasm !== undefined) return wasm;

    let thread_stack_size
    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path, memory, thread_stack_size} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('kithara-ffi_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports(memory);

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module, thread_stack_size);
}

export { initSync, __wbg_init as default };


// --- Auto-generated by xtask wasm postbuild ---

/**
 * Check if the browser environment supports SharedArrayBuffer + COEP/COOP.
 * Returns { ok: boolean, reason?: string, waitingForReload?: boolean }.
 */
export function checkRuntime() {
    const crossOriginIsolated = self.crossOriginIsolated === true;
    const secureContext = self.isSecureContext === true;
    const sharedArrayBuffer = typeof SharedArrayBuffer !== 'undefined';

    if (secureContext && sharedArrayBuffer && crossOriginIsolated) {
        sessionStorage.removeItem('kithara_coi_reloaded');
        return { ok: true };
    }

    const waitingForReload =
        secureContext && !crossOriginIsolated &&
        typeof navigator.serviceWorker !== 'undefined' &&
        !navigator.serviceWorker.controller;

    if (waitingForReload) {
        navigator.serviceWorker.ready.then(() => {
            if (navigator.serviceWorker.controller || self.crossOriginIsolated === true) {
                sessionStorage.removeItem('kithara_coi_reloaded');
                return;
            }
            if (sessionStorage.getItem('kithara_coi_reloaded') === '1') return;
            sessionStorage.setItem('kithara_coi_reloaded', '1');
            window.location.reload();
        }).catch(() => {});
        return { ok: false, waitingForReload: true, reason: 'Waiting for COI service worker to activate' };
    }

    return {
        ok: false,
        waitingForReload: false,
        reason: `secureContext=${secureContext} crossOriginIsolated=${crossOriginIsolated} sharedArrayBuffer=${sharedArrayBuffer}`,
    };
}
