#[cfg(any(
    not(any(
        feature = "client-reqwest",
        feature = "client-wreq",
        feature = "client-apple",
        feature = "client-host"
    )),
    all(target_arch = "wasm32", not(feature = "client-reqwest"))
))]
compile_error!(
    "kithara-net: enable at least one HTTP client backend; wasm32 requires `client-reqwest`"
);

#[cfg(all(
    feature = "client-reqwest",
    not(target_arch = "wasm32"),
    not(any(feature = "tls-rustls", feature = "tls-native"))
))]
compile_error!("kithara-net: `client-reqwest` needs `tls-rustls` or `tls-native`");
