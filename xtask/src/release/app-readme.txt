Kithara for macOS

Requires macOS 13 or later. This universal application contains Apple Silicon
and Intel code. Move Kithara.app to Applications or another writable folder.
The application includes its UI package, default skin, pictures and licenses.
The initial library is empty: add your own audio files through the application.
Developer credentials are not included.
User configuration is stored outside the signed application at
~/Library/Application Support/Kithara/kithara.yaml.

The application is signed with the certificate selected by its builder.
This archive has not been notarized. An Apple Development signature verifies
who signed these bytes, but does not grant Developer ID distribution trust.
Gatekeeper can block a downloaded copy. On macOS versions that offer it, use
System Settings > Privacy & Security > Open Anyway after checking the sender
and archive checksum. Managed Macs may prohibit that override.

For public distribution without that manual approval, the publisher needs a
Developer ID Application certificate and Apple notarization. This build does
not claim either. Do not disable Gatekeeper globally.

Modifying anything inside the application invalidates its signature. Supply
an external configuration with --config or an external UI package with
--ui-package when experimenting with a different skin.
