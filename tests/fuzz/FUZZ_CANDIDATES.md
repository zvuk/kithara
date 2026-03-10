# Fuzz-кандидаты в Kithara (без изменения production-кода)

Ниже перечислены участки, где можно увеличить покрытие через fuzzing, не меняя runtime-логику библиотек. Идея — добавить только новые fuzz targets в `tests/fuzz/fuzz_targets/`.

## 1) `kithara-storage`: `MemDriver::write_at` / `read_at`

**Где:** `crates/kithara-storage/src/memory.rs`

**Почему это хороший fuzz-таргет:**
- есть арифметика смещений и длин (`offset + data.len() as u64`), где легко получить граничные значения;
- есть конверсия `u64 -> usize` и разная логика для sparse/обычных записей;
- поведение зависит от флагов состояния ресурса (committed vs non-committed).

**Что проверять инвариантами:**
- отсутствие panic на произвольной последовательности операций `write/read/commit`;
- `read_at` никогда не возвращает байты за пределами уже записанной длины;
- после `commit` запись корректно отклоняется ожидаемой ошибкой.

## 2) `kithara-hls`: `parse_media_playlist` и пост-обработка ключей

**Где:** `crates/kithara-hls/src/parsing.rs`, `crates/kithara-hls/src/keys.rs`

**Почему это хороший fuzz-таргет:**
- текущий fuzz покрывает только `hls_m3u8` parser напрямую, но не внутреннюю пост-обработку Kithara;
- в пост-обработке есть дополнительные ветки: `current_key`, `init_segment`, `detected_container`, `allow_cache`, `end_list`;
- в `KeyManager::resolve_key_url` и `rel_path_from_url` есть нормализация URL/путей, чувствительная к необычным строкам.

**Что проверять инвариантами:**
- отсутствие panic на произвольных плейлистах/URI;
- для любого `KeyInfo` с непустым URI `resolve_key_url` возвращает либо валидный `Url`, либо typed error;
- `derive_iv` всегда возвращает массив из 16 байт и не паникует на любых `sequence`.

## 3) `kithara-stream`: state-machine чтения/seek в `Stream<T>`

**Где:** `crates/kithara-stream/src/stream.rs`

**Почему это хороший fuzz-таргет:**
- сложная комбинация `wait_range` + `read_at` + `Timeline` с retry/interrupted/eof сценариями;
- отдельные ветви для `SeekFrom::{Start, Current, End}` и проверки границ;
- логика важна для корректного восстановления декодера и поведения при seek.

**Что проверять инвариантами:**
- отсутствие panic на случайных сценариях возврата `WaitOutcome`/`ReadOutcome`;
- `seek` корректно ограничивает отрицательные и out-of-bounds позиции;
- позиция потока (`timeline.byte_position`) остается согласованной после ошибок и retry.

## 4) `kithara-play`: входной парсинг `ResourceConfig::new`

**Где:** `crates/kithara-play/src/impls/config.rs`

**Почему это хороший fuzz-таргет:**
- функция принимает произвольную пользовательскую строку и делает ветвление URL vs filesystem path;
- есть платформо-зависимые ветки (`wasm32`/native) и special-case для `file://`;
- хорошая зона для hardening against malformed input и unicode/path corner-cases.

**Что проверять инвариантами:**
- отсутствие panic на произвольных строках;
- результат всегда либо валидный `ResourceConfig`, либо typed `DecodeError::InvalidData`;
- для корректных абсолютных путей и валидных URL поведение стабильно.

---

## Приоритет запуска

1. `kithara-storage` (`MemDriver`) — максимальная плотность граничной арифметики.
2. `kithara-hls` (внутренняя пост-обработка + keys) — закрыть gap относительно текущего fuzz `hls_m3u8`.
3. `kithara-stream` (read/seek state-machine) — высокая важность для runtime-стабильности.
4. `kithara-play` (`ResourceConfig::new`) — hardening публичной входной точки.

## 5) `kithara-assets`: `asset_root_for_url` + `ResourceKey::from_url`

**Где:** `crates/kithara-assets/src/key.rs`

**Почему это хороший fuzz-таргет:**
- хеш-идентификатор строится через каноникализацию URL (scheme/host/port/query/fragment), что чувствительно к edge-case строкам;
- `ResourceKey::from_url` извлекает path segment и должен стабильно отрабатывать на нестандартных URI;
- обе функции принимают внешние данные (URL и имя), значит важна устойчивость к произвольному input.

**Что проверять инвариантами:**
- отсутствие panic на валидных произвольных URL;
- `asset_root_for_url` всегда возвращает фиксированный hex-идентификатор длиной 32;
- удаление query/fragment не меняет `asset_root_for_url` для URL с host;
- `ResourceKey::from_url` всегда возвращает относительный ключ для URL-входа.
