# kithara-test-dylib

One shared object that carries the product and harness graph every test binary
would otherwise link statically. On Android it also carries `kithara-app`
(`lib-only`). Binaries that opt in keep that code in a single image: depend on
this crate and import it so the linker sees the dylib.
