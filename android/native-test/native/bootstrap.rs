#![no_std]

use core::ffi::c_void;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn init_context(vm: *mut c_void, context: *mut c_void) {
    unsafe { ndk_context::initialize_android_context(vm, context) };
}
