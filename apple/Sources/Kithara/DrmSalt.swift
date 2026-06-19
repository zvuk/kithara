import KitharaFFI

/// Generate the zvuk.com production DRM salt: 8 lowercase-hex characters.
public func drmProdSalt() -> String {
    KitharaFFI.drmProdSalt()
}

/// Generate the zvq.me staging DRM salt: 16 ASCII alphanumeric characters.
public func drmStageSalt() -> String {
    KitharaFFI.drmStageSalt()
}
