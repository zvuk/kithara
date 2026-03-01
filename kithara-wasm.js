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
     * @param {string} url
     * @returns {number}
     */
    add_track(url) {
        const ptr0 = passStringToWasm0(url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmplayer_add_track(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * @returns {number}
     */
    current_index() {
        const ret = wasm.wasmplayer_current_index(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {string}
     */
    static default_file_url() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmplayer_default_file_url();
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {string}
     */
    static default_hls_url() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmplayer_default_hls_url();
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
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
     * @param {number} index
     * @returns {string}
     */
    playlist_item(index) {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmplayer_playlist_item(this.__wbg_ptr, index);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * @returns {number}
     */
    playlist_len() {
        const ret = wasm.wasmplayer_playlist_len(this.__wbg_ptr);
        return ret >>> 0;
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
     * @param {number} index
     * @returns {Promise<void>}
     */
    select_track(index) {
        const ret = wasm.wasmplayer_select_track(this.__wbg_ptr, index);
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
     * Diagnostic: read HLS stream bytes without audio decoding.
     * Runs read loop in the Worker thread.
     */
    test_hls_read() {
        wasm.wasmplayer_test_hls_read(this.__wbg_ptr);
    }
    /**
     * Poll session commands from Workers and update the audio graph.
     *
     * Must be called unconditionally — even before a track is selected —
     * because `select_track` sends session-engine commands from the Worker
     * that the main thread must process for the call to complete.
     */
    tick() {
        const ret = wasm.wasmplayer_tick(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
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
 * Diagnostic: return current WASM linear memory size in bytes.
 * @returns {number}
 */
export function wasm_memory_bytes() {
    const ret = wasm.wasm_memory_bytes();
    return ret;
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
        __wbg___wbindgen_debug_string_6cf0badf0b90f6ef: function(arg0, arg1) {
            const ret = debugString(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_4500d4795b15e70b: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_undefined_1296fcc83c2da07a: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_memory_bb52d7a9276ec5cc: function() {
            const ret = wasm.memory;
            return ret;
        },
        __wbg___wbindgen_module_e057ebc1440a8fb0: function() {
            const ret = wasmModule;
            return ret;
        },
        __wbg___wbindgen_number_get_3330675b4e5c3680: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_rethrow_fb3a909a1a998bce: function(arg0) {
            throw arg0;
        },
        __wbg___wbindgen_string_get_7b8bc463f6cbeefe: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_89ca9e2c67795ec1: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_f00ff3c6385bd6fa: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_abort_d5982476775d2739: function(arg0, arg1) {
            arg0.abort(arg1);
        },
        __wbg_abort_e6a92d5623297220: function(arg0) {
            arg0.abort();
        },
        __wbg_addEventListener_aa4bf6d5347ab364: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.addEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_addModule_12d9e8d44f6db2a3: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.addModule(getStringFromWasm0(arg1, arg2));
            return ret;
        }, arguments); },
        __wbg_append_bddb95024c591a53: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.append(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_arrayBuffer_c95edc576c2724fe: function() { return handleError(function (arg0) {
            const ret = arg0.arrayBuffer();
            return ret;
        }, arguments); },
        __wbg_async_a4aacdc143453890: function(arg0) {
            const ret = arg0.async;
            return ret;
        },
        __wbg_audioWorklet_8287e4556c42ae76: function() { return handleError(function (arg0) {
            const ret = arg0.audioWorklet;
            return ret;
        }, arguments); },
        __wbg_body_dbcb65a77cb7d1cd: function(arg0) {
            const ret = arg0.body;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_buffer_b5caa4cad053be8b: function(arg0) {
            const ret = arg0.buffer;
            return ret;
        },
        __wbg_buffer_bfc0004d8d490b64: function(arg0) {
            const ret = arg0.buffer;
            return ret;
        },
        __wbg_byobRequest_8eeabe8afa525bca: function(arg0) {
            const ret = arg0.byobRequest;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_byteLength_7439036992d87511: function(arg0) {
            const ret = arg0.byteLength;
            return ret;
        },
        __wbg_byteLength_bf98fbeb47223e5a: function(arg0) {
            const ret = arg0.byteLength;
            return ret;
        },
        __wbg_byteOffset_fb5b318131582b8a: function(arg0) {
            const ret = arg0.byteOffset;
            return ret;
        },
        __wbg_call_3eadb5cea0462653: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_cancel_ab703655e38608f0: function(arg0) {
            const ret = arg0.cancel();
            return ret;
        },
        __wbg_catch_ea008a796e986e6a: function(arg0, arg1) {
            const ret = arg0.catch(arg1);
            return ret;
        },
        __wbg_clearTimeout_2256f1e7b94ef517: function(arg0) {
            const ret = clearTimeout(arg0);
            return ret;
        },
        __wbg_close_2e7d01b33743e3d6: function() { return handleError(function (arg0) {
            arg0.close();
        }, arguments); },
        __wbg_close_5c4196c455e152a3: function() { return handleError(function (arg0) {
            const ret = arg0.close();
            return ret;
        }, arguments); },
        __wbg_close_be2ea2e967b2d21e: function() { return handleError(function (arg0) {
            arg0.close();
        }, arguments); },
        __wbg_connect_39174dd1966d0e62: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.connect(arg1);
            return ret;
        }, arguments); },
        __wbg_createObjectURL_585b06cf88b17b6f: function() { return handleError(function (arg0, arg1) {
            const ret = URL.createObjectURL(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_data_946ee98fc7c8524e: function(arg0) {
            const ret = arg0.data;
            return ret;
        },
        __wbg_destination_274bb6deda5d1dcd: function(arg0) {
            const ret = arg0.destination;
            return ret;
        },
        __wbg_disconnect_a033714255067c31: function() { return handleError(function (arg0) {
            arg0.disconnect();
        }, arguments); },
        __wbg_document_3b31159a83fa664b: function(arg0) {
            const ret = arg0.document;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_done_82b14aeb31e98db6: function(arg0) {
            const ret = arg0.done;
            return ret;
        },
        __wbg_enqueue_eca5b83d6c551f60: function() { return handleError(function (arg0, arg1) {
            arg0.enqueue(arg1);
        }, arguments); },
        __wbg_entries_279f7c28f13d750a: function(arg0) {
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
        __wbg_fetch_c170c157e1a111d4: function(arg0, arg1) {
            const ret = arg0.fetch(arg1);
            return ret;
        },
        __wbg_getReader_9facd4f899beac89: function() { return handleError(function (arg0) {
            const ret = arg0.getReader();
            return ret;
        }, arguments); },
        __wbg_getUserMedia_21a1aa1d706781cd: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.getUserMedia(arg1);
            return ret;
        }, arguments); },
        __wbg_get_229657ec2da079cd: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_get_done_c355ff5cc3338368: function(arg0) {
            const ret = arg0.done;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg_get_value_e7da3dda29ab004c: function(arg0) {
            const ret = arg0.value;
            return ret;
        },
        __wbg_has_01b31fbd88bb3e8f: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.has(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_headers_fa752b79db86f8b3: function(arg0) {
            const ret = arg0.headers;
            return ret;
        },
        __wbg_instanceof_Float32Array_1f1a7732e795dc29: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Float32Array;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_MediaStream_46c8093aef48a722: function(arg0) {
            let result;
            try {
                result = arg0 instanceof MediaStream;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Response_1844be67dbd5e161: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Response;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_27a653e1b516dd65: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_isArray_fe5201bfdab7e39d: function(arg0) {
            const ret = Array.isArray(arg0);
            return ret;
        },
        __wbg_length_5e79666440f4af1e: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_length_f875d3a041bab91a: function(arg0) {
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
        __wbg_log_240aa86e7eb48d31: function(arg0) {
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
        __wbg_mediaDevices_2d31d0604d1546a5: function() { return handleError(function (arg0) {
            const ret = arg0.mediaDevices;
            return ret;
        }, arguments); },
        __wbg_message_8410b1a862148c1a: function(arg0, arg1) {
            const ret = arg1.message;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_navigator_373a54253e4475fb: function(arg0) {
            const ret = arg0.navigator;
            return ret;
        },
        __wbg_new_0496ae3a63d32b2f: function() { return handleError(function (arg0, arg1) {
            const ret = new MediaStreamAudioSourceNode(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_new_08d33c204155a3e2: function() { return handleError(function () {
            const ret = new AbortController();
            return ret;
        }, arguments); },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return ret;
        },
        __wbg_new_6e7681a5f6f98ceb: function(arg0) {
            const ret = new Uint8Array(arg0);
            return ret;
        },
        __wbg_new_6feff3e11e4d0799: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_88d5c88956d5309b: function() { return handleError(function () {
            const ret = new lAudioContext();
            return ret;
        }, arguments); },
        __wbg_new_bfabfaa6b6feafcf: function(arg0, arg1) {
            const ret = new Error(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_d42238ad1cfcc2b8: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___js_sys_3849cb6a8b3aa6c2___Function_fn_wasm_bindgen_ac95679beaebdfdd___JsValue_____wasm_bindgen_ac95679beaebdfdd___sys__Undefined___js_sys_3849cb6a8b3aa6c2___Function_fn_wasm_bindgen_ac95679beaebdfdd___JsValue_____wasm_bindgen_ac95679beaebdfdd___sys__Undefined______(a, state0.b, arg0, arg1);
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
        __wbg_new_e88efd8ca5aef956: function() { return handleError(function () {
            const ret = new Headers();
            return ret;
        }, arguments); },
        __wbg_new_ede48eb358ad8574: function(arg0) {
            const ret = new Int32Array(arg0);
            return ret;
        },
        __wbg_new_fdf9d977ebec93ca: function() { return handleError(function (arg0, arg1) {
            const ret = new Worker(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_new_from_slice_a5be53238f31f9f7: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_typed_f79896f0ea5f7de8: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___js_sys_3849cb6a8b3aa6c2___Function_fn_wasm_bindgen_ac95679beaebdfdd___JsValue_____wasm_bindgen_ac95679beaebdfdd___sys__Undefined___js_sys_3849cb6a8b3aa6c2___Function_fn_wasm_bindgen_ac95679beaebdfdd___JsValue_____wasm_bindgen_ac95679beaebdfdd___sys__Undefined______(a, state0.b, arg0, arg1);
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
        __wbg_new_with_byte_offset_and_length_df68ba89684072a3: function(arg0, arg1, arg2) {
            const ret = new Uint8Array(arg0, arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_new_with_context_options_eb11d90fa6c27367: function() { return handleError(function (arg0) {
            const ret = new lAudioContext(arg0);
            return ret;
        }, arguments); },
        __wbg_new_with_options_b5e0aa35e848e57e: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = new AudioWorkletNode(arg0, getStringFromWasm0(arg1, arg2), arg3);
            return ret;
        }, arguments); },
        __wbg_new_with_str_and_init_66de5344635d3590: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = new Request(getStringFromWasm0(arg0, arg1), arg2);
            return ret;
        }, arguments); },
        __wbg_new_with_str_sequence_and_options_bc6d2b6f78a49214: function() { return handleError(function (arg0, arg1) {
            const ret = new Blob(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_next_ae5b710aea83f41e: function() { return handleError(function (arg0) {
            const ret = arg0.next();
            return ret;
        }, arguments); },
        __wbg_now_e7c6795a7f81e10f: function(arg0) {
            const ret = arg0.now();
            return ret;
        },
        __wbg_of_46d2833daab368cb: function(arg0, arg1) {
            const ret = Array.of(arg0, arg1);
            return ret;
        },
        __wbg_of_80583358c221d1d1: function(arg0, arg1, arg2) {
            const ret = Array.of(arg0, arg1, arg2);
            return ret;
        },
        __wbg_of_fcc1c8c0899b3729: function(arg0) {
            const ret = Array.of(arg0);
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
        __wbg_postMessage_34f706fb38829973: function() { return handleError(function (arg0, arg1) {
            arg0.postMessage(arg1);
        }, arguments); },
        __wbg_prototypesetcall_37f00e1be5c4015a: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_prototypesetcall_e37423ff3fb72fc7: function(arg0, arg1, arg2) {
            Float32Array.prototype.set.call(getArrayF32FromWasm0(arg0, arg1), arg2);
        },
        __wbg_queueMicrotask_5e387cf4d8e3f63e: function(arg0) {
            queueMicrotask(arg0);
        },
        __wbg_queueMicrotask_77bf5a3ad712168b: function(arg0) {
            const ret = arg0.queueMicrotask;
            return ret;
        },
        __wbg_read_4cfc9fe040a376a9: function(arg0) {
            const ret = arg0.read();
            return ret;
        },
        __wbg_releaseLock_5793b32569461e8e: function(arg0) {
            arg0.releaseLock();
        },
        __wbg_removeEventListener_0e6b66e033b2bc59: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.removeEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_resolve_2e8556632715b12f: function(arg0) {
            const ret = Promise.resolve(arg0);
            return ret;
        },
        __wbg_respond_46abc75dbd3694f0: function() { return handleError(function (arg0, arg1) {
            arg0.respond(arg1 >>> 0);
        }, arguments); },
        __wbg_resume_0d07c790b9f8b973: function() { return handleError(function (arg0) {
            const ret = arg0.resume();
            return ret;
        }, arguments); },
        __wbg_sampleRate_c0c63105703b0392: function(arg0) {
            const ret = arg0.sampleRate;
            return ret;
        },
        __wbg_setTimeout_b188b3bcc8977c7d: function(arg0, arg1) {
            const ret = setTimeout(arg0, arg1);
            return ret;
        },
        __wbg_set_200e8fd4c20f90ff: function(arg0, arg1, arg2) {
            arg0.set(getArrayF32FromWasm0(arg1, arg2));
        },
        __wbg_set_76943c82a5e79352: function(arg0, arg1, arg2) {
            arg0.set(getArrayU8FromWasm0(arg1, arg2));
        },
        __wbg_set_audio_8316e2b9b1428999: function(arg0, arg1) {
            arg0.audio = arg1;
        },
        __wbg_set_body_42d5ed933a1840a1: function(arg0, arg1) {
            arg0.body = arg1;
        },
        __wbg_set_cache_546c3dda0e43ae0c: function(arg0, arg1) {
            arg0.cache = __wbindgen_enum_RequestCache[arg1];
        },
        __wbg_set_channel_count_0d7b23d711612568: function(arg0, arg1) {
            arg0.channelCount = arg1 >>> 0;
        },
        __wbg_set_credentials_328fbf29cfdfa342: function(arg0, arg1) {
            arg0.credentials = __wbindgen_enum_RequestCredentials[arg1];
        },
        __wbg_set_headers_1f8bdee11d576059: function(arg0, arg1) {
            arg0.headers = arg1;
        },
        __wbg_set_media_stream_c8097e5a8db59c18: function(arg0, arg1) {
            arg0.mediaStream = arg1;
        },
        __wbg_set_method_5a35896632ca213d: function(arg0, arg1, arg2) {
            arg0.method = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_mode_6fa10db5d133ac26: function(arg0, arg1) {
            arg0.mode = __wbindgen_enum_RequestMode[arg1];
        },
        __wbg_set_number_of_inputs_46a56d5bee11a0f1: function(arg0, arg1) {
            arg0.numberOfInputs = arg1 >>> 0;
        },
        __wbg_set_number_of_outputs_21c5c968d217c7c6: function(arg0, arg1) {
            arg0.numberOfOutputs = arg1 >>> 0;
        },
        __wbg_set_onmessage_c0e9c77376920dd7: function(arg0, arg1) {
            arg0.onmessage = arg1;
        },
        __wbg_set_onprocessorerror_7cafd777df99f85b: function(arg0, arg1) {
            arg0.onprocessorerror = arg1;
        },
        __wbg_set_output_channel_count_21966c47c6f6f247: function(arg0, arg1) {
            arg0.outputChannelCount = arg1;
        },
        __wbg_set_processor_options_4b3e1a3db72173f4: function(arg0, arg1) {
            arg0.processorOptions = arg1;
        },
        __wbg_set_sample_rate_7c32b349255118f8: function(arg0, arg1) {
            arg0.sampleRate = arg1;
        },
        __wbg_set_signal_0d93209e168efc85: function(arg0, arg1) {
            arg0.signal = arg1;
        },
        __wbg_set_type_80ffbd25eee49008: function(arg0, arg1, arg2) {
            arg0.type = getStringFromWasm0(arg1, arg2);
        },
        __wbg_signal_a0d9fead74a07e34: function(arg0) {
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
        __wbg_state_6e6d33cfcdc01c0c: function(arg0) {
            const ret = arg0.state;
            return (__wbindgen_enum_AudioContextState.indexOf(ret) + 1 || 4) - 1;
        },
        __wbg_static_accessor_GLOBAL_280fe6a619bbfbf6: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_12c1f4811ec605d1: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_IMPORT_META_a4b382c4359199c3: function() {
            const ret = import.meta;
            return ret;
        },
        __wbg_static_accessor_SELF_3a156961626f54d9: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_210015b3eb6018a4: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_status_1544422a8c64aef0: function(arg0) {
            const ret = arg0.status;
            return ret;
        },
        __wbg_suspend_b73231eb36a20e5e: function() { return handleError(function (arg0) {
            const ret = arg0.suspend();
            return ret;
        }, arguments); },
        __wbg_text_667415a14b08e789: function() { return handleError(function (arg0) {
            const ret = arg0.text();
            return ret;
        }, arguments); },
        __wbg_then_0a414b9f42c97f7a: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_then_5ce48a9e69c0d3cd: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_then_f73127af3894d61c: function(arg0, arg1, arg2) {
            const ret = arg0.then(arg1, arg2);
            return ret;
        },
        __wbg_timeOrigin_f3d5cb4f4a06c2b7: function(arg0) {
            const ret = arg0.timeOrigin;
            return ret;
        },
        __wbg_url_271945e420831f05: function(arg0, arg1) {
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
        __wbg_value_3e1fdb73e1353fb3: function(arg0) {
            const ret = arg0.value;
            return ret;
        },
        __wbg_value_8bfc87a10af4c75b: function(arg0) {
            const ret = arg0.value;
            return ret;
        },
        __wbg_view_9c320e46c7c72feb: function(arg0) {
            const ret = arg0.view;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_waitAsync_de33c243df61ccb9: function() {
            const ret = Atomics.waitAsync;
            return ret;
        },
        __wbg_waitAsync_f1f3dda14fef3ea8: function(arg0, arg1, arg2) {
            const ret = Atomics.waitAsync(arg0, arg1 >>> 0, arg2);
            return ret;
        },
        __wbg_wasm_safe_thread_spawn_worker_691b5a5ca7ddcb3d: function(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7) {
            const ret = wasm_safe_thread_spawn_worker(arg0, arg1, arg2, getStringFromWasm0(arg3, arg4), getStringFromWasm0(arg5, arg6), arg7 >>> 0);
            return ret;
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2363, function: Function { arguments: [], shim_idx: 2364, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_ac95679beaebdfdd___closure__destroy___dyn_core_81cc84ba46089792___ops__function__FnMut_____Output_______, wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke______);
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2422, function: Function { arguments: [Externref], shim_idx: 2423, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_ac95679beaebdfdd___closure__destroy___dyn_core_81cc84ba46089792___ops__function__FnMut__wasm_bindgen_ac95679beaebdfdd___JsValue____Output_______, wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue______1_);
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2604, function: Function { arguments: [U32], shim_idx: 2605, ret: Unit, inner_ret: Some(Unit) }, mutable: false }) -> Externref`.
            const ret = makeClosure(arg0, arg1, wasm.wasm_bindgen_ac95679beaebdfdd___closure__destroy___dyn_core_81cc84ba46089792___ops__function__Fn__u32____Output_______, wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___u32_____);
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2614, function: Function { arguments: [Externref], shim_idx: 2615, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_ac95679beaebdfdd___closure__destroy___dyn_core_81cc84ba46089792___ops__function__FnMut__wasm_bindgen_ac95679beaebdfdd___JsValue____Output___core_81cc84ba46089792___result__Result_____wasm_bindgen_ac95679beaebdfdd___JsError___, wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue__core_81cc84ba46089792___result__Result_____wasm_bindgen_ac95679beaebdfdd___JsError__);
            return ret;
        },
        __wbindgen_cast_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 2614, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 2617, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_ac95679beaebdfdd___closure__destroy___dyn_core_81cc84ba46089792___ops__function__FnMut__wasm_bindgen_ac95679beaebdfdd___JsValue____Output___core_81cc84ba46089792___result__Result_____wasm_bindgen_ac95679beaebdfdd___JsError___, wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___web_sys_dc36d93627ada558___features__gen_MessageEvent__MessageEvent_____);
            return ret;
        },
        __wbindgen_cast_0000000000000006: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 623, function: Function { arguments: [Externref], shim_idx: 624, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_ac95679beaebdfdd___closure__destroy___dyn_core_81cc84ba46089792___ops__function__Fn__web_sys_dc36d93627ada558___features__gen_ErrorEvent__ErrorEvent____Output_______, wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue_____);
            return ret;
        },
        __wbindgen_cast_0000000000000007: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 623, function: Function { arguments: [NamedExternref("ErrorEvent")], shim_idx: 626, ret: Unit, inner_ret: Some(Unit) }, mutable: false }) -> Externref`.
            const ret = makeClosure(arg0, arg1, wasm.wasm_bindgen_ac95679beaebdfdd___closure__destroy___dyn_core_81cc84ba46089792___ops__function__Fn__web_sys_dc36d93627ada558___features__gen_ErrorEvent__ErrorEvent____Output_______, wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___web_sys_dc36d93627ada558___features__gen_ErrorEvent__ErrorEvent_____);
            return ret;
        },
        __wbindgen_cast_0000000000000008: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 623, function: Function { arguments: [NamedExternref("Event")], shim_idx: 624, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_ac95679beaebdfdd___closure__destroy___dyn_core_81cc84ba46089792___ops__function__Fn__web_sys_dc36d93627ada558___features__gen_ErrorEvent__ErrorEvent____Output_______, wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue______7);
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
        __wbindgen_link_8075785854a611bd: function(arg0) {
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
function wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke______(arg0, arg1) {
    wasm.wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke______(arg0, arg1);
}

function wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue______1_(arg0, arg1, arg2) {
    wasm.wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue______1_(arg0, arg1, arg2);
}

function wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___web_sys_dc36d93627ada558___features__gen_MessageEvent__MessageEvent_____(arg0, arg1, arg2) {
    wasm.wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___web_sys_dc36d93627ada558___features__gen_MessageEvent__MessageEvent_____(arg0, arg1, arg2);
}

function wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue_____(arg0, arg1, arg2) {
    wasm.wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue_____(arg0, arg1, arg2);
}

function wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___web_sys_dc36d93627ada558___features__gen_ErrorEvent__ErrorEvent_____(arg0, arg1, arg2) {
    wasm.wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___web_sys_dc36d93627ada558___features__gen_ErrorEvent__ErrorEvent_____(arg0, arg1, arg2);
}

function wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue______7(arg0, arg1, arg2) {
    wasm.wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue______7(arg0, arg1, arg2);
}

function wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue__core_81cc84ba46089792___result__Result_____wasm_bindgen_ac95679beaebdfdd___JsError__(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___wasm_bindgen_ac95679beaebdfdd___JsValue__core_81cc84ba46089792___result__Result_____wasm_bindgen_ac95679beaebdfdd___JsError__(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___js_sys_3849cb6a8b3aa6c2___Function_fn_wasm_bindgen_ac95679beaebdfdd___JsValue_____wasm_bindgen_ac95679beaebdfdd___sys__Undefined___js_sys_3849cb6a8b3aa6c2___Function_fn_wasm_bindgen_ac95679beaebdfdd___JsValue_____wasm_bindgen_ac95679beaebdfdd___sys__Undefined______(arg0, arg1, arg2, arg3) {
    wasm.wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___js_sys_3849cb6a8b3aa6c2___Function_fn_wasm_bindgen_ac95679beaebdfdd___JsValue_____wasm_bindgen_ac95679beaebdfdd___sys__Undefined___js_sys_3849cb6a8b3aa6c2___Function_fn_wasm_bindgen_ac95679beaebdfdd___JsValue_____wasm_bindgen_ac95679beaebdfdd___sys__Undefined______(arg0, arg1, arg2, arg3);
}

function wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___u32_____(arg0, arg1, arg2) {
    wasm.wasm_bindgen_ac95679beaebdfdd___convert__closures_____invoke___u32_____(arg0, arg1, arg2);
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
