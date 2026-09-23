# JNA's native code resolves Pointer.peer and Structure fields by name.
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { *; }

# JNA's AWT bridge is unreachable on Android.
-dontwarn java.awt.**

# JNA binds the generated methods to exported symbols by name and reads structures by field.
-keep class com.kithara.ffi.** { *; }

# The JNI entry points Java_com_kithara_Kithara_* spell the class and its native methods.
-keepclasseswithmembernames,includedescriptorclasses class com.kithara.Kithara {
    native <methods>;
}

# The native library calls the transport by name, finds and constructs the
# callback, and exports the natives of both classes by class and method name.
-keep interface com.kithara.net.HttpTransport { *; }
-keep interface com.kithara.net.HttpCall { *; }
-keep interface com.kithara.net.HttpCallback
-keep class com.kithara.net.NativeHttpCallback {
    <init>(long);
    native <methods>;
}
-keep class com.kithara.net.NativeHttpTransport {
    native <methods>;
}
