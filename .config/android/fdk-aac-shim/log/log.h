// `fdk-aac-sys` vendors the AOSP snapshot of libfdk-aac, and one guard in
// `libSBRdec/src/lpp_tran.cpp` reports a rejected SBR patch layout to the
// platform's safetynet channel. Both the header it includes and
// `android_errorWriteLog` live in AOSP's liblog and reach no NDK sysroot, so
// the Android lanes put this directory on the include path.
//
// The report is telemetry the decoder ignores; the guarded branch carries
// nothing else.
#pragma once

static inline int android_errorWriteLog(int tag, const char *subTag) {
  (void)tag;
  (void)subTag;
  return 0;
}
