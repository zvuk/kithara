# Decoder Architecture Design

## Overview

Новая архитектура декодеров для `kithara-decode` — generic/trait-based система с поддержкой аппаратных (Apple AudioConverter, Android MediaCodec) и программных (Symphonia) декодеров.

## Goals

- Убрать зависимость от `FormatReader`/probe в пользу packet-first подхода
- Единый `AudioDecoder` trait для всех backend'ов
- Типобезопасный выбор кодека через generics
- Runtime fallback через `DecoderFactory`
- Mock декодер через unimock для тестирования

## Core Trait

```rust
use unimock::unimock;

#[unimock(api = AudioDecoderMock)]
pub trait AudioDecoder: Send + 'static {
    /// Конфигурация, специфичная для реализации
    type Config: Default;

    /// Создание декодера из любого Read + Seek источника
    fn new<R>(source: R, config: Self::Config) -> Result<Self, DecodeError>
    where
        R: Read + Seek + Send + 'static,
        Self: Sized;

    /// Декодировать следующий chunk PCM данных
    fn next_chunk(&mut self) -> Result<Option<PcmChunk>, DecodeError>;

    /// Спецификация выходного PCM
    fn spec(&self) -> PcmSpec;

    /// Seek к временной позиции
    fn seek(&mut self, pos: Duration) -> Result<(), DecodeError>;

    /// Текущая позиция воспроизведения
    fn position(&self) -> Duration;

    /// Общая длительность (если известна)
    fn duration(&self) -> Option<Duration> {
        None
    }
}
```

## Codec Type System

```rust
/// Marker trait для кодеков
pub trait CodecType: Send + 'static {
    const CODEC: AudioCodec;
}

// Marker types
pub struct Aac;
pub struct Mp3;
pub struct Flac;
pub struct Alac;

impl CodecType for Aac { const CODEC: AudioCodec = AudioCodec::Aac; }
impl CodecType for Mp3 { const CODEC: AudioCodec = AudioCodec::Mp3; }
impl CodecType for Flac { const CODEC: AudioCodec = AudioCodec::Flac; }
impl CodecType for Alac { const CODEC: AudioCodec = AudioCodec::Alac; }
```

## Decoder Types

### Naming Convention

Backend + Codec: `SymphoniaAac`, `AppleAac`, `AndroidAac`

### Structure

```rust
// Generic decoder с inner implementation
pub struct Symphonia<C: CodecType> {
    inner: SymphoniaInner,
    _codec: PhantomData<C>,
}

// Type aliases
pub type SymphoniaAac = Symphonia<Aac>;
pub type SymphoniaMp3 = Symphonia<Mp3>;
pub type SymphoniaFlac = Symphonia<Flac>;

// Аналогично для Apple и Android
pub type AppleAac = Apple<Aac>;
pub type AppleMp3 = Apple<Mp3>;
pub type AndroidAac = Android<Aac>;
pub type AndroidMp3 = Android<Mp3>;
```

## Configuration

```rust
/// Общая конфигурация для DecoderFactory
#[derive(Debug, Clone, Default)]
pub struct DecoderConfig {
    pub prefer_hardware: bool,
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    pub gapless: bool,
}

/// Symphonia-specific
#[derive(Debug, Clone, Default)]
pub struct SymphoniaConfig {
    pub verify: bool,
    pub gapless: bool,
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

/// Apple-specific
#[derive(Debug, Clone, Default)]
pub struct AppleConfig {
    pub hardware_preferred: bool,
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

/// Android-specific
#[derive(Debug, Clone, Default)]
pub struct AndroidConfig {
    pub hardware_preferred: bool,
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}
```

## DecoderFactory

```rust
pub enum CodecSelector {
    /// Известный кодек — без probe
    Exact(AudioCodec),
    /// Probe с подсказками
    Probe(ProbeHint),
    /// Полный auto-probe
    Auto,
}

#[derive(Debug, Clone, Default)]
pub struct ProbeHint {
    pub codec: Option<AudioCodec>,
    pub container: Option<ContainerFormat>,
    pub extension: Option<String>,
    pub mime: Option<String>,
}

pub struct DecoderFactory;

impl DecoderFactory {
    pub fn create<R>(
        source: R,
        selector: CodecSelector,
        config: DecoderConfig,
    ) -> Result<Box<dyn AudioDecoder>, DecodeError>
    where
        R: Read + Seek + Send + 'static;
}
```

Runtime fallback: hardware (если `prefer_hardware`) → Symphonia.

## Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Unsupported codec: {0:?}")]
    UnsupportedCodec(AudioCodec),

    #[error("Unsupported container: {0:?}")]
    UnsupportedContainer(ContainerFormat),

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Seek failed: {0}")]
    SeekFailed(String),

    #[error("Probe failed: could not detect codec")]
    ProbeFailed,

    #[error("Decoder error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}
```

## Data Types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmSpec {
    pub sample_rate: u32,
    pub channels: u16,
}

pub struct PcmChunk {
    pub samples: PooledBuffer<f32>,
    pub spec: PcmSpec,
    pub timestamp: Duration,
}
```

## File Structure

```
kithara-decode/src/
├── lib.rs              // Реэкспорты, feature gates
├── types.rs            // PcmChunk, PcmSpec
├── error.rs            // DecodeError, DecodeResult
├── traits.rs           // AudioDecoder, CodecType, marker types
├── factory.rs          // DecoderFactory, CodecSelector, ProbeHint
├── symphonia.rs        // Symphonia<C>, SymphoniaInner, SymphoniaConfig
├── apple.rs            // Apple<C>, AppleInner, AppleConfig
└── android.rs          // Android<C>, AndroidInner, AndroidConfig
```

## Cargo.toml Features

```toml
[features]
default = ["symphonia"]
symphonia = ["dep:symphonia", "dep:symphonia-core"]
apple = ["dep:coreaudio-sys"]
android = ["dep:mediacodec"]

[dev-dependencies]
unimock = "0.6"
```

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────┐
│                    DecoderFactory                        │
│  create(source, CodecSelector, DecoderConfig)           │
│         → Box<dyn AudioDecoder>                         │
└─────────────────┬───────────────────────────────────────┘
                  │
    ┌─────────────┼─────────────┬─────────────┐
    ▼             ▼             ▼             ▼
┌────────┐  ┌────────┐    ┌────────┐    ┌────────┐
│Symphonia│  │ Apple  │    │Android │    │  Mock  │
│  <C>    │  │  <C>   │    │  <C>   │    │(unimock)│
└────────┘  └────────┘    └────────┘    └────────┘
    │             │             │
    ▼             ▼             ▼
┌────────┐  ┌────────┐    ┌────────┐
│Symphonia│  │ Audio  │    │ Media  │
│ Inner   │  │Converter│   │ Codec  │
└────────┘  └────────┘    └────────┘
```

## Usage Examples

```rust
// Прямое использование конкретного декодера
let decoder = SymphoniaAac::new(stream, SymphoniaConfig::default())?;

// Через factory с автовыбором
let decoder = DecoderFactory::create(
    stream,
    CodecSelector::Exact(AudioCodec::Aac),
    DecoderConfig { prefer_hardware: true, ..Default::default() },
)?;

// С probe и hint
let decoder = DecoderFactory::create(
    stream,
    CodecSelector::Probe(ProbeHint {
        extension: Some("m4a".into()),
        ..Default::default()
    }),
    DecoderConfig::default(),
)?;
```

## Backend Implementation Notes

### Symphonia

- `SymphoniaInner` содержит `PacketReader` trait object (AdtsReader, MpaReader, FlacReader)
- `PacketReader` абстрагирует codec-specific readers
- `decode()` вызывается на `symphonia_core::codecs::Decoder::decode(&Packet)`

### Apple (AudioConverter)

- `AppleInner` — wrapper над `AudioConverterRef`
- `AudioConverterNew()` создаёт конвертер из input format в PCM output
- `AudioConverterFillComplexBuffer()` с callback для декодирования
- Seek = `AudioConverterReset()` + seek в source

### Android (MediaCodec)

- `AndroidInner` — wrapper над `MediaCodec`
- `MediaCodec::create_decoder(mime_type)` создаёт декодер
- Input/output buffer pattern
- Seek = flush + seek в source

## Key Decisions

1. **One generic parameter**: `Decoder<SymphoniaAac>` вместо `Decoder<Aac, Symphonia>`
2. **Backend-first naming**: `SymphoniaAac`, не `AacSymphonia`
3. **Associated type for Config**: типобезопасная конфигурация для каждого backend
4. **Generic `new<R>()`**: принимает любой source, type erasure внутри
5. **Feature flags + runtime fallback**: compile-time gating + `DecoderFactory` для автовыбора
6. **Unified `create()` with CodecSelector**: Exact, Probe, Auto режимы
7. **Flat module structure**: один файл на backend
8. **Seek in main trait**: обязателен для ABR
9. **Byte length через Config**: `Arc<AtomicU64>` передаётся при создании
10. **Generic Backend error**: `DecodeError::Backend(Box<dyn Error>)`

## Research Sources

- [Symphonia](https://github.com/pdeljanov/Symphonia) — pure Rust audio decoding
- [coreaudio-sys](https://github.com/RustAudio/coreaudio-sys) — Apple CoreAudio bindings
- [mediacodec](https://crates.io/crates/mediacodec) — Android MediaCodec bindings
- [unimock](https://crates.io/crates/unimock) — mock generation for traits
