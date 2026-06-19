import KitharaFFI

/// Generate an 8-character lowercase-hex DRM salt.
public func drmLowercaseHexSalt() -> String {
    KitharaFFI.drmLowercaseHexSalt()
}

/// Generate a 16-character ASCII alphanumeric DRM salt.
public func drmAsciiAlphanumericSalt() -> String {
    KitharaFFI.drmAsciiAlphanumericSalt()
}
