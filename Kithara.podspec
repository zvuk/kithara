Pod::Spec.new do |s|
  s.name = 'Kithara'
  s.version = '0.0.1-alpha3'
  s.summary = 'Cross-platform streaming audio player'
  s.description = 'Kithara is a Rust-backed streaming audio player for iOS with HLS, progressive playback, DRM hooks, and adaptive bitrate support.'
  s.homepage = 'https://github.com/zvuk/kithara'
  s.license = { :type => 'MIT OR Apache-2.0' }
  s.authors = { 'kithara contributors' => 'zvuk_ai@prosoftware.io' }
  s.source = {
    :http => 'https://github.com/zvuk/kithara/releases/download/v0.0.1-alpha3/Kithara.xcframework.zip',
    :sha256 => '104d51891cdf94049a069b52fb21e89146d826ecd80efbdc9f2b64148e0f29d7'
  }
  s.module_name = 'Kithara'
  s.swift_versions = ['6.0']
  s.ios.deployment_target = '16.0'
  s.vendored_frameworks = 'Kithara.xcframework'
  s.frameworks = 'AudioToolbox', 'CoreAudio', 'AVFoundation', 'Security', 'SystemConfiguration'
  s.requires_arc = true
end
