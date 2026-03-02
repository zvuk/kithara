import { wasm_safe_thread_spawn_worker } from './snippets/wasm_safe_thread-6af64f6626d30659/inline0.js';
import { park_notify_at_addr, park_wait_at_addr, park_wait_timeout_at_addr } from './snippets/wasm_safe_thread-6af64f6626d30659/inline2.js';

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
        const ret = wasm.intounderlyingbytesource_pull(this.__wbg_ptr, controller);
        return ret;
    }
    /**
     * @param {ReadableByteStreamController} controller
     */
    start(controller) {
        wasm.intounderlyingbytesource_start(this.__wbg_ptr, controller);
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
        const ret = wasm.intounderlyingsink_abort(ptr, reason);
        return ret;
    }
    /**
     * @returns {Promise<any>}
     */
    close() {
        const ptr = this.__destroy_into_raw();
        const ret = wasm.intounderlyingsink_close(ptr);
        return ret;
    }
    /**
     * @param {any} chunk
     * @returns {Promise<any>}
     */
    write(chunk) {
        const ret = wasm.intounderlyingsink_write(this.__wbg_ptr, chunk);
        return ret;
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
        const ret = wasm.intounderlyingsource_pull(this.__wbg_ptr, controller);
        return ret;
    }
}
if (Symbol.dispose) IntoUnderlyingSource.prototype[Symbol.dispose] = IntoUnderlyingSource.prototype.free;

export class ProcessorHost {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
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
        const ret = wasm.processorhost_process(this.__wbg_ptr, inputs, outputs, current_time);
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

export class WasmPlayer {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmPlayerFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmplayer_free(ptr, 0);
    }
    /**
     * @returns {number}
     */
    eq_band_count() {
        const ret = wasm.wasmplayer_eq_band_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @param {number} band
     * @returns {number}
     */
    eq_gain(band) {
        const ret = wasm.wasmplayer_eq_gain(this.__wbg_ptr, band);
        return ret;
    }
    /**
     * @returns {number}
     */
    get_crossfade_seconds() {
        const ret = wasm.wasmplayer_get_crossfade_seconds(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {number}
     */
    get_duration_ms() {
        const ret = wasm.wasmplayer_get_duration_ms(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {number}
     */
    get_position_ms() {
        const ret = wasm.wasmplayer_get_position_ms(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {number}
     */
    get_session_ducking() {
        const ret = wasm.wasmplayer_get_session_ducking(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @returns {number}
     */
    get_volume() {
        const ret = wasm.wasmplayer_get_volume(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {boolean}
     */
    is_playing() {
        const ret = wasm.wasmplayer_is_playing(this.__wbg_ptr);
        return ret !== 0;
    }
    constructor() {
        const ret = wasm.wasmplayer_new();
        this.__wbg_ptr = ret >>> 0;
        WasmPlayerFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    pause() {
        wasm.wasmplayer_pause(this.__wbg_ptr);
    }
    play() {
        wasm.wasmplayer_play(this.__wbg_ptr);
    }
    /**
     * @returns {number}
     */
    process_count() {
        const ret = wasm.wasmplayer_process_count(this.__wbg_ptr);
        return ret;
    }
    reset_eq() {
        const ret = wasm.wasmplayer_reset_eq(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * @param {number} position_ms
     */
    seek(position_ms) {
        const ret = wasm.wasmplayer_seek(this.__wbg_ptr, position_ms);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * @param {string} url
     * @returns {Promise<void>}
     */
    select_track(url) {
        const ptr0 = passStringToWasm0(url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmplayer_select_track(this.__wbg_ptr, ptr0, len0);
        return ret;
    }
    /**
     * @param {number} seconds
     */
    set_crossfade_seconds(seconds) {
        wasm.wasmplayer_set_crossfade_seconds(this.__wbg_ptr, seconds);
    }
    /**
     * @param {number} band
     * @param {number} gain_db
     */
    set_eq_gain(band, gain_db) {
        const ret = wasm.wasmplayer_set_eq_gain(this.__wbg_ptr, band, gain_db);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * @param {number} mode
     */
    set_session_ducking(mode) {
        const ret = wasm.wasmplayer_set_session_ducking(this.__wbg_ptr, mode);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * @param {number} volume
     */
    set_volume(volume) {
        wasm.wasmplayer_set_volume(this.__wbg_ptr, volume);
    }
    stop() {
        wasm.wasmplayer_stop(this.__wbg_ptr);
    }
    /**
     * @returns {string}
     */
    take_events() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmplayer_take_events(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Poll session commands from Workers and update the audio graph.
     *
     * Must be called unconditionally — even before a track is selected —
     * because `select_track` sends session-engine commands from the Worker
     * that the main thread must process for the call to complete.
     */
    tick() {
        wasm.wasmplayer_tick(this.__wbg_ptr);
    }
}
if (Symbol.dispose) WasmPlayer.prototype[Symbol.dispose] = WasmPlayer.prototype.free;

/**
 * Build revision string: "version git_hash build_timestamp"
 * @returns {string}
 */
export function build_info() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.build_info();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

export function setup() {
    wasm.setup();
}

/**
 * @param {any} work
 */
export function wasm_safe_thread_entry_point(work) {
    wasm.wasm_safe_thread_entry_point(work);
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
import * as import1 from "./snippets/wasm_safe_thread-6af64f6626d30659/inline1.js"
import * as import2 from "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js"
import * as import3 from "./snippets/wasm_safe_thread-6af64f6626d30659/inline0.js"
import * as import4 from "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js"
import * as import5 from "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js"
import * as import6 from "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js"

function __wbg_get_imports(memory) {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_debug_string_5398f5bb970e0daa: function(arg0, arg1) {
            const ret = debugString(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_3c846841762788c1: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_undefined_52709e72fb9f179c: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_memory_edb3f01e3930bbf6: function() {
            const ret = wasm.memory;
            return ret;
        },
        __wbg___wbindgen_module_bf945c07123bafe2: function() {
            const ret = wasmModule;
            return ret;
        },
        __wbg___wbindgen_number_get_34bb9d9dcfa21373: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_rethrow_5d3a9250cec92549: function(arg0) {
            throw arg0;
        },
        __wbg___wbindgen_string_get_395e606bd0ee4427: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_6ddd609b62940d55: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_6b5b6b8576d35cb1: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_abort_5ef96933660780b7: function(arg0) {
            arg0.abort();
        },
        __wbg_abort_6479c2d794ebf2ee: function(arg0, arg1) {
            arg0.abort(arg1);
        },
        __wbg_addEventListener_2d985aa8a656f6dc: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.addEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_addModule_803558c991bff401: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.addModule(getStringFromWasm0(arg1, arg2));
            return ret;
        }, arguments); },
        __wbg_append_608dfb635ee8998f: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.append(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_arrayBuffer_eb8e9ca620af2a19: function() { return handleError(function (arg0) {
            const ret = arg0.arrayBuffer();
            return ret;
        }, arguments); },
        __wbg_async_b33e4cb28c6b2093: function(arg0) {
            const ret = arg0.async;
            return ret;
        },
        __wbg_audioWorklet_b37c738d39d2b3fe: function() { return handleError(function (arg0) {
            const ret = arg0.audioWorklet;
            return ret;
        }, arguments); },
        __wbg_body_ac1dad652946e6da: function(arg0) {
            const ret = arg0.body;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_buffer_60b8043cd926067d: function(arg0) {
            const ret = arg0.buffer;
            return ret;
        },
        __wbg_buffer_eb2779983eb67380: function(arg0) {
            const ret = arg0.buffer;
            return ret;
        },
        __wbg_byobRequest_6342e5f2b232c0f9: function(arg0) {
            const ret = arg0.byobRequest;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_byteLength_607b856aa6c5a508: function(arg0) {
            const ret = arg0.byteLength;
            return ret;
        },
        __wbg_byteOffset_b26b63681c83856c: function(arg0) {
            const ret = arg0.byteOffset;
            return ret;
        },
        __wbg_call_2d781c1f4d5c0ef8: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_cancel_79b3bea07a1028e7: function(arg0) {
            const ret = arg0.cancel();
            return ret;
        },
        __wbg_catch_d7ed0375ab6532a5: function(arg0, arg1) {
            const ret = arg0.catch(arg1);
            return ret;
        },
        __wbg_clearTimeout_2256f1e7b94ef517: function(arg0) {
            const ret = clearTimeout(arg0);
            return ret;
        },
        __wbg_close_690d36108c557337: function() { return handleError(function (arg0) {
            arg0.close();
        }, arguments); },
        __wbg_close_737b4b1fbc658540: function() { return handleError(function (arg0) {
            arg0.close();
        }, arguments); },
        __wbg_close_87218c1c5fa30509: function() { return handleError(function (arg0) {
            const ret = arg0.close();
            return ret;
        }, arguments); },
        __wbg_connect_3ca85e8e3b8d9828: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.connect(arg1);
            return ret;
        }, arguments); },
        __wbg_createObjectURL_f141426bcc1f70aa: function() { return handleError(function (arg0, arg1) {
            const ret = URL.createObjectURL(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_data_a3d9ff9cdd801002: function(arg0) {
            const ret = arg0.data;
            return ret;
        },
        __wbg_destination_d1f70fe081ff0932: function(arg0) {
            const ret = arg0.destination;
            return ret;
        },
        __wbg_disconnect_e6c7e0fcdcbf45bf: function() { return handleError(function (arg0) {
            arg0.disconnect();
        }, arguments); },
        __wbg_document_c0320cd4183c6d9b: function(arg0) {
            const ret = arg0.document;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_done_08ce71ee07e3bd17: function(arg0) {
            const ret = arg0.done;
            return ret;
        },
        __wbg_enqueue_ec3552838b4b7fbf: function() { return handleError(function (arg0, arg1) {
            arg0.enqueue(arg1);
        }, arguments); },
        __wbg_entries_5b8fe91cea59610e: function(arg0) {
            const ret = arg0.entries();
            return ret;
        },
        __wbg_error_620e7bbed54be6fa: function(arg0, arg1) {
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
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_fetch_43b2f110608a59ff: function(arg0) {
            const ret = fetch(arg0);
            return ret;
        },
        __wbg_fetch_5550a88cf343aaa9: function(arg0, arg1) {
            const ret = arg0.fetch(arg1);
            return ret;
        },
        __wbg_getReader_9facd4f899beac89: function() { return handleError(function (arg0) {
            const ret = arg0.getReader();
            return ret;
        }, arguments); },
        __wbg_getUserMedia_3c429b74c1afa2c0: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.getUserMedia(arg1);
            return ret;
        }, arguments); },
        __wbg_get_a8ee5c45dabc1b3b: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_get_done_d0ab690f8df5501f: function(arg0) {
            const ret = arg0.done;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg_get_value_548ae6adf5a174e4: function(arg0) {
            const ret = arg0.value;
            return ret;
        },
        __wbg_has_926ef2ff40b308cf: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.has(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_headers_eb2234545f9ff993: function(arg0) {
            const ret = arg0.headers;
            return ret;
        },
        __wbg_instanceof_Float32Array_6f3d6ec9510b72a8: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Float32Array;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_MediaStream_cb811cd532c4d6c3: function(arg0) {
            let result;
            try {
                result = arg0 instanceof MediaStream;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Response_9b4d9fd451e051b1: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Response;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_23e677d2c6843922: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_isArray_33b91feb269ff46e: function(arg0) {
            const ret = Array.isArray(arg0);
            return ret;
        },
        __wbg_length_259ee9d041e381ad: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_length_ea16607d7b61445b: function(arg0) {
            const ret = arg0.length;
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
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_log_524eedafa26daa59: function(arg0) {
            console.log(arg0);
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
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
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
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
                wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
            }
        }, arguments); },
        __wbg_mediaDevices_aaefe1de72ee0411: function() { return handleError(function (arg0) {
            const ret = arg0.mediaDevices;
            return ret;
        }, arguments); },
        __wbg_message_67f6368dc2a526af: function(arg0, arg1) {
            const ret = arg1.message;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_navigator_9cebf56f28aa719b: function(arg0) {
            const ret = arg0.navigator;
            return ret;
        },
        __wbg_new_0837727332ac86ba: function() { return handleError(function () {
            const ret = new Headers();
            return ret;
        }, arguments); },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return ret;
        },
        __wbg_new_3acd383af1655b5f: function() { return handleError(function (arg0, arg1) {
            const ret = new Worker(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_new_5e532409c6c7bba4: function() { return handleError(function () {
            const ret = new lAudioContext();
            return ret;
        }, arguments); },
        __wbg_new_5f486cdf45a04d78: function(arg0) {
            const ret = new Uint8Array(arg0);
            return ret;
        },
        __wbg_new_ab79df5bd7c26067: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_af04f4c3ed7fd887: function(arg0) {
            const ret = new Int32Array(arg0);
            return ret;
        },
        __wbg_new_c518c60af666645b: function() { return handleError(function () {
            const ret = new AbortController();
            return ret;
        }, arguments); },
        __wbg_new_d098e265629cd10f: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___js_sys_383b22ca621a8488___Function_fn_wasm_bindgen_b324b76982e8d137___JsValue_____wasm_bindgen_b324b76982e8d137___sys__Undefined___js_sys_383b22ca621a8488___Function_fn_wasm_bindgen_b324b76982e8d137___JsValue_____wasm_bindgen_b324b76982e8d137___sys__Undefined_______true_(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return ret;
            } finally {
                state0.a = state0.b = 0;
            }
        },
        __wbg_new_d15cb560a6a0e5f0: function(arg0, arg1) {
            const ret = new Error(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_da8abbe7498a895c: function() { return handleError(function (arg0, arg1) {
            const ret = new MediaStreamAudioSourceNode(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_new_from_slice_22da9388ac046e50: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_typed_aaaeaf29cf802876: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___js_sys_383b22ca621a8488___Function_fn_wasm_bindgen_b324b76982e8d137___JsValue_____wasm_bindgen_b324b76982e8d137___sys__Undefined___js_sys_383b22ca621a8488___Function_fn_wasm_bindgen_b324b76982e8d137___JsValue_____wasm_bindgen_b324b76982e8d137___sys__Undefined_______true_(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return ret;
            } finally {
                state0.a = state0.b = 0;
            }
        },
        __wbg_new_with_byte_offset_and_length_b2ec5bf7b2f35743: function(arg0, arg1, arg2) {
            const ret = new Uint8Array(arg0, arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_new_with_context_options_c1249ea1a7ddc84f: function() { return handleError(function (arg0) {
            const ret = new lAudioContext(arg0);
            return ret;
        }, arguments); },
        __wbg_new_with_options_e8d476233ad4514c: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = new AudioWorkletNode(arg0, getStringFromWasm0(arg1, arg2), arg3);
            return ret;
        }, arguments); },
        __wbg_new_with_str_and_init_b4b54d1a819bc724: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = new Request(getStringFromWasm0(arg0, arg1), arg2);
            return ret;
        }, arguments); },
        __wbg_new_with_str_sequence_and_options_a037535f6e1edba0: function() { return handleError(function (arg0, arg1) {
            const ret = new Blob(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_next_11b99ee6237339e3: function() { return handleError(function (arg0) {
            const ret = arg0.next();
            return ret;
        }, arguments); },
        __wbg_now_e7c6795a7f81e10f: function(arg0) {
            const ret = arg0.now();
            return ret;
        },
        __wbg_of_8bf7ed3eca00ea43: function(arg0) {
            const ret = Array.of(arg0);
            return ret;
        },
        __wbg_of_8fd5dd402bc67165: function(arg0, arg1, arg2) {
            const ret = Array.of(arg0, arg1, arg2);
            return ret;
        },
        __wbg_of_d6376e3774c51f89: function(arg0, arg1) {
            const ret = Array.of(arg0, arg1);
            return ret;
        },
        __wbg_park_notify_at_addr_078552077bb71254: function(arg0, arg1) {
            park_notify_at_addr(arg0, arg1 >>> 0);
        },
        __wbg_park_wait_at_addr_f11d67a9f8b3c8f9: function(arg0, arg1) {
            const ret = park_wait_at_addr(arg0, arg1 >>> 0);
            return ret;
        },
        __wbg_park_wait_timeout_at_addr_c086eefff696f392: function(arg0, arg1, arg2) {
            const ret = park_wait_timeout_at_addr(arg0, arg1 >>> 0, arg2);
            return ret;
        },
        __wbg_performance_3fcf6e32a7e1ed0a: function(arg0) {
            const ret = arg0.performance;
            return ret;
        },
        __wbg_postMessage_edb4c90a528e5a8c: function() { return handleError(function (arg0, arg1) {
            arg0.postMessage(arg1);
        }, arguments); },
        __wbg_prototypesetcall_247ac4333d4d3cb4: function(arg0, arg1, arg2) {
            Float32Array.prototype.set.call(getArrayF32FromWasm0(arg0, arg1), arg2);
        },
        __wbg_prototypesetcall_d62e5099504357e6: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_queueMicrotask_0c399741342fb10f: function(arg0) {
            const ret = arg0.queueMicrotask;
            return ret;
        },
        __wbg_queueMicrotask_a082d78ce798393e: function(arg0) {
            queueMicrotask(arg0);
        },
        __wbg_read_7f593a961a7f80ed: function(arg0) {
            const ret = arg0.read();
            return ret;
        },
        __wbg_releaseLock_ef7766a5da654ff8: function(arg0) {
            arg0.releaseLock();
        },
        __wbg_removeEventListener_d27694700fc0df8b: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.removeEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_resolve_ae8d83246e5bcc12: function(arg0) {
            const ret = Promise.resolve(arg0);
            return ret;
        },
        __wbg_respond_e286ee502e7cf7e4: function() { return handleError(function (arg0, arg1) {
            arg0.respond(arg1 >>> 0);
        }, arguments); },
        __wbg_resume_7cf56c82bfdf6c58: function() { return handleError(function (arg0) {
            const ret = arg0.resume();
            return ret;
        }, arguments); },
        __wbg_sampleRate_5d9f421731d2459d: function(arg0) {
            const ret = arg0.sampleRate;
            return ret;
        },
        __wbg_setTimeout_b188b3bcc8977c7d: function(arg0, arg1) {
            const ret = setTimeout(arg0, arg1);
            return ret;
        },
        __wbg_set_361bc2460da3016f: function(arg0, arg1, arg2) {
            arg0.set(getArrayF32FromWasm0(arg1, arg2));
        },
        __wbg_set_8c0b3ffcf05d61c2: function(arg0, arg1, arg2) {
            arg0.set(getArrayU8FromWasm0(arg1, arg2));
        },
        __wbg_set_audio_d55dfa4d70f98e62: function(arg0, arg1) {
            arg0.audio = arg1;
        },
        __wbg_set_body_a3d856b097dfda04: function(arg0, arg1) {
            arg0.body = arg1;
        },
        __wbg_set_cache_ec7e430c6056ebda: function(arg0, arg1) {
            arg0.cache = __wbindgen_enum_RequestCache[arg1];
        },
        __wbg_set_channel_count_35640061cc490a51: function(arg0, arg1) {
            arg0.channelCount = arg1 >>> 0;
        },
        __wbg_set_credentials_ed63183445882c65: function(arg0, arg1) {
            arg0.credentials = __wbindgen_enum_RequestCredentials[arg1];
        },
        __wbg_set_headers_3c8fecc693b75327: function(arg0, arg1) {
            arg0.headers = arg1;
        },
        __wbg_set_media_stream_f4a9f79a7b139ba5: function(arg0, arg1) {
            arg0.mediaStream = arg1;
        },
        __wbg_set_method_8c015e8bcafd7be1: function(arg0, arg1, arg2) {
            arg0.method = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_mode_5a87f2c809cf37c2: function(arg0, arg1) {
            arg0.mode = __wbindgen_enum_RequestMode[arg1];
        },
        __wbg_set_number_of_inputs_6b354879aedcac2d: function(arg0, arg1) {
            arg0.numberOfInputs = arg1 >>> 0;
        },
        __wbg_set_number_of_outputs_63331a7a3868717f: function(arg0, arg1) {
            arg0.numberOfOutputs = arg1 >>> 0;
        },
        __wbg_set_onmessage_d5dc11c291025af6: function(arg0, arg1) {
            arg0.onmessage = arg1;
        },
        __wbg_set_onprocessorerror_e7634704351d4022: function(arg0, arg1) {
            arg0.onprocessorerror = arg1;
        },
        __wbg_set_output_channel_count_50557afa7ccb7c00: function(arg0, arg1) {
            arg0.outputChannelCount = arg1;
        },
        __wbg_set_processor_options_87cc3f9ddfa9ea93: function(arg0, arg1) {
            arg0.processorOptions = arg1;
        },
        __wbg_set_sample_rate_88fa12f3b8a6ae94: function(arg0, arg1) {
            arg0.sampleRate = arg1;
        },
        __wbg_set_signal_0cebecb698f25d21: function(arg0, arg1) {
            arg0.signal = arg1;
        },
        __wbg_set_type_33e79f1b45a78c37: function(arg0, arg1, arg2) {
            arg0.type = getStringFromWasm0(arg1, arg2);
        },
        __wbg_signal_166e1da31adcac18: function(arg0) {
            const ret = arg0.signal;
            return ret;
        },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = arg1.stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_state_a4d9e52dfc1783cb: function(arg0) {
            const ret = arg0.state;
            return (__wbindgen_enum_AudioContextState.indexOf(ret) + 1 || 4) - 1;
        },
        __wbg_static_accessor_GLOBAL_8adb955bd33fac2f: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_ad356e0db91c7913: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_IMPORT_META_a4b382c4359199c3: function() {
            const ret = import.meta;
            return ret;
        },
        __wbg_static_accessor_SELF_f207c857566db248: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_bb9f1ba69d61b386: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_status_318629ab93a22955: function(arg0) {
            const ret = arg0.status;
            return ret;
        },
        __wbg_suspend_7c95f73fe1803f5b: function() { return handleError(function (arg0) {
            const ret = arg0.suspend();
            return ret;
        }, arguments); },
        __wbg_text_372f5b91442c50f9: function() { return handleError(function (arg0) {
            const ret = arg0.text();
            return ret;
        }, arguments); },
        __wbg_then_098abe61755d12f6: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_then_1d7a5273811a5cea: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_then_9e335f6dd892bc11: function(arg0, arg1, arg2) {
            const ret = arg0.then(arg1, arg2);
            return ret;
        },
        __wbg_timeOrigin_f3d5cb4f4a06c2b7: function(arg0) {
            const ret = arg0.timeOrigin;
            return ret;
        },
        __wbg_url_7fefc1820fba4e0c: function(arg0, arg1) {
            const ret = arg1.url;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_url_89d3827eb946d99a: function(arg0) {
            const ret = arg0.url;
            return ret;
        },
        __wbg_value_21fc78aab0322612: function(arg0) {
            const ret = arg0.value;
            return ret;
        },
        __wbg_value_a529cd2f781749fd: function(arg0) {
            const ret = arg0.value;
            return ret;
        },
        __wbg_view_f68a712e7315f8b2: function(arg0) {
            const ret = arg0.view;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_waitAsync_91ab9cf292b5ab15: function(arg0, arg1, arg2) {
            const ret = Atomics.waitAsync(arg0, arg1 >>> 0, arg2);
            return ret;
        },
        __wbg_waitAsync_a4399d51368b6ce4: function() {
            const ret = Atomics.waitAsync;
            return ret;
        },
        __wbg_wasm_safe_thread_spawn_worker_691b5a5ca7ddcb3d: function(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7) {
            const ret = wasm_safe_thread_spawn_worker(arg0, arg1, arg2, getStringFromWasm0(arg3, arg4), getStringFromWasm0(arg5, arg6), arg7 >>> 0);
            return ret;
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2341, function: Function { arguments: [], shim_idx: 2342, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_b324b76982e8d137___closure__destroy___dyn_core_81cc84ba46089792___ops__function__FnMut_____Output_______, wasm_bindgen_b324b76982e8d137___convert__closures_____invoke_______true_);
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2395, function: Function { arguments: [Externref], shim_idx: 2396, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_b324b76982e8d137___closure__destroy___dyn_core_81cc84ba46089792___ops__function__FnMut__wasm_bindgen_b324b76982e8d137___JsValue____Output_______, wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue______true__1_);
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2581, function: Function { arguments: [U32], shim_idx: 2582, ret: Unit, inner_ret: Some(Unit) }, mutable: false }) -> Externref`.
            const ret = makeClosure(arg0, arg1, wasm.wasm_bindgen_b324b76982e8d137___closure__destroy___dyn_core_81cc84ba46089792___ops__function__Fn__u32____Output_______, wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___u32______true_);
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2596, function: Function { arguments: [Externref], shim_idx: 2597, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_b324b76982e8d137___closure__destroy___dyn_core_81cc84ba46089792___ops__function__FnMut__wasm_bindgen_b324b76982e8d137___JsValue____Output___core_81cc84ba46089792___result__Result_____wasm_bindgen_b324b76982e8d137___JsError___, wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue__core_81cc84ba46089792___result__Result_____wasm_bindgen_b324b76982e8d137___JsError___true_);
            return ret;
        },
        __wbindgen_cast_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2596, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 2599, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_b324b76982e8d137___closure__destroy___dyn_core_81cc84ba46089792___ops__function__FnMut__wasm_bindgen_b324b76982e8d137___JsValue____Output___core_81cc84ba46089792___result__Result_____wasm_bindgen_b324b76982e8d137___JsError___, wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___web_sys_3eaa0db1ceef978f___features__gen_MessageEvent__MessageEvent______true_);
            return ret;
        },
        __wbindgen_cast_0000000000000006: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 599, function: Function { arguments: [Externref], shim_idx: 602, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_b324b76982e8d137___closure__destroy___dyn_core_81cc84ba46089792___ops__function__Fn__web_sys_3eaa0db1ceef978f___features__gen_ErrorEvent__ErrorEvent____Output_______, wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue______true_);
            return ret;
        },
        __wbindgen_cast_0000000000000007: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 599, function: Function { arguments: [NamedExternref("ErrorEvent")], shim_idx: 600, ret: Unit, inner_ret: Some(Unit) }, mutable: false }) -> Externref`.
            const ret = makeClosure(arg0, arg1, wasm.wasm_bindgen_b324b76982e8d137___closure__destroy___dyn_core_81cc84ba46089792___ops__function__Fn__web_sys_3eaa0db1ceef978f___features__gen_ErrorEvent__ErrorEvent____Output_______, wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___web_sys_3eaa0db1ceef978f___features__gen_ErrorEvent__ErrorEvent______true_);
            return ret;
        },
        __wbindgen_cast_0000000000000008: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 599, function: Function { arguments: [NamedExternref("Event")], shim_idx: 602, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_b324b76982e8d137___closure__destroy___dyn_core_81cc84ba46089792___ops__function__Fn__web_sys_3eaa0db1ceef978f___features__gen_ErrorEvent__ErrorEvent____Output_______, wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue______true__7);
            return ret;
        },
        __wbindgen_cast_0000000000000009: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_000000000000000a: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
        __wbindgen_link_fcd7cf2a23e346d3: function(arg0) {
            const val = `onmessage = function (ev) {
                let [ia, index, value] = ev.data;
                ia = new Int32Array(ia.buffer);
                let result = Atomics.wait(ia, index, value);
                postMessage(result);
            };
            `;
            const ret = typeof URL.createObjectURL === 'undefined' ? "data:application/javascript," + encodeURIComponent(val) : URL.createObjectURL(new Blob([val], { type: "text/javascript" }));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        memory: memory || new WebAssembly.Memory({initial:25,maximum:24576,shared:true}),
    };
    return {
        __proto__: null,
        "./kithara-wasm_bg.js": import0,
        "./snippets/wasm_safe_thread-6af64f6626d30659/inline1.js": import1,
        "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js": import2,
        "./snippets/wasm_safe_thread-6af64f6626d30659/inline0.js": import3,
        "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js": import4,
        "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js": import5,
        "./snippets/wasm_safe_thread-6af64f6626d30659/inline2.js": import6,
    };
}

const lAudioContext = (typeof AudioContext !== 'undefined' ? AudioContext : (typeof webkitAudioContext !== 'undefined' ? webkitAudioContext : undefined));
function wasm_bindgen_b324b76982e8d137___convert__closures_____invoke_______true_(arg0, arg1) {
    wasm.wasm_bindgen_b324b76982e8d137___convert__closures_____invoke_______true_(arg0, arg1);
}

function wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue______true__1_(arg0, arg1, arg2) {
    wasm.wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue______true__1_(arg0, arg1, arg2);
}

function wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___web_sys_3eaa0db1ceef978f___features__gen_MessageEvent__MessageEvent______true_(arg0, arg1, arg2) {
    wasm.wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___web_sys_3eaa0db1ceef978f___features__gen_MessageEvent__MessageEvent______true_(arg0, arg1, arg2);
}

function wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue______true_(arg0, arg1, arg2) {
    wasm.wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue______true_(arg0, arg1, arg2);
}

function wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___web_sys_3eaa0db1ceef978f___features__gen_ErrorEvent__ErrorEvent______true_(arg0, arg1, arg2) {
    wasm.wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___web_sys_3eaa0db1ceef978f___features__gen_ErrorEvent__ErrorEvent______true_(arg0, arg1, arg2);
}

function wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue______true__7(arg0, arg1, arg2) {
    wasm.wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue______true__7(arg0, arg1, arg2);
}

function wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue__core_81cc84ba46089792___result__Result_____wasm_bindgen_b324b76982e8d137___JsError___true_(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___wasm_bindgen_b324b76982e8d137___JsValue__core_81cc84ba46089792___result__Result_____wasm_bindgen_b324b76982e8d137___JsError___true_(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___js_sys_383b22ca621a8488___Function_fn_wasm_bindgen_b324b76982e8d137___JsValue_____wasm_bindgen_b324b76982e8d137___sys__Undefined___js_sys_383b22ca621a8488___Function_fn_wasm_bindgen_b324b76982e8d137___JsValue_____wasm_bindgen_b324b76982e8d137___sys__Undefined_______true_(arg0, arg1, arg2, arg3) {
    wasm.wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___js_sys_383b22ca621a8488___Function_fn_wasm_bindgen_b324b76982e8d137___JsValue_____wasm_bindgen_b324b76982e8d137___sys__Undefined___js_sys_383b22ca621a8488___Function_fn_wasm_bindgen_b324b76982e8d137___JsValue_____wasm_bindgen_b324b76982e8d137___sys__Undefined_______true_(arg0, arg1, arg2, arg3);
}

function wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___u32______true_(arg0, arg1, arg2) {
    wasm.wasm_bindgen_b324b76982e8d137___convert__closures_____invoke___u32______true_(arg0, arg1, arg2);
}


const __wbindgen_enum_AudioContextState = ["suspended", "running", "closed"];


const __wbindgen_enum_ReadableStreamType = ["bytes"];


const __wbindgen_enum_RequestCache = ["default", "no-store", "reload", "no-cache", "force-cache", "only-if-cached"];


const __wbindgen_enum_RequestCredentials = ["omit", "same-origin", "include"];


const __wbindgen_enum_RequestMode = ["same-origin", "no-cors", "cors", "navigate"];
const IntoUnderlyingByteSourceFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_intounderlyingbytesource_free(ptr >>> 0, 1));
const IntoUnderlyingSinkFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_intounderlyingsink_free(ptr >>> 0, 1));
const IntoUnderlyingSourceFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_intounderlyingsource_free(ptr >>> 0, 1));
const ProcessorHostFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_processorhost_free(ptr >>> 0, 1));
const WasmPlayerFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmplayer_free(ptr >>> 0, 1));

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => state.dtor(state.a, state.b));

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
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.buffer !== wasm.memory.buffer) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeClosure(arg0, arg1, dtor, f) {
    const state = { a: arg0, b: arg1, cnt: 1, dtor };
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
            state.dtor(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function makeMutClosure(arg0, arg1, dtor, f) {
    const state = { a: arg0, b: arg1, cnt: 1, dtor };
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
            state.dtor(state.a, state.b);
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

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
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

let wasmModule, wasm;
function __wbg_finalize_init(instance, module, thread_stack_size) {
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
        module_or_path = new URL('kithara-wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports(memory);

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module, thread_stack_size);
}

export { initSync, __wbg_init as default };
