# ABR reference spec (portable) — estimator/controller/decisions/tests

Этот документ — **самодостаточный reference** для реализации ABR в `kithara-hls` агентами, которые **не имеют доступа** к legacy-коду (`stream-download-hls`).
Цель — получить ABR, который:
- детерминированно тестируется локальными фикстурами,
- разделяет **decision vs applied**,
- **не обновляет throughput по cache hits** (важно для offline),
- реализует базовые guard-rails: hysteresis, safety factor, min switch interval, buffer gating.

Связанные документы:
- `docs/constraints.md` (особенно раздел “ABR: cache hits не должны улучшать throughput”)
- `docs/kanban-kithara-hls.md`
- `docs/porting/legacy-reference.md`

---

## 1) Термины и модель данных

### 1.1 Variant model

ABR работает с набором variants из master playlist.

Минимально нужные поля для каждого variant:

- `variant_index: usize` — индекс в списке variants (стабильный внутри сессии).
- `bandwidth_bps: u64` — заявленная полоса (HLS `BANDWIDTH`), бит/сек.
- `resolution: Option<(u32,u32)>` — опционально (может использоваться как tie-break).
- `codec_group: Option<String>` — опционально, если нужно понимать “смену кодека”.

Рекомендация:
- Сначала ранжируй по `bandwidth_bps`.
- При равенстве можно tie-break по resolution или порядку в плейлисте.

### 1.2 Throughput sample

Через ABR проходят измерения throughput, но **не все** из них должны влиять на estimator.

```text
ThroughputSample {
  bytes: u64,
  duration: Duration,   // wall time, затраченное на network download
  at: Instant,          // timestamp, когда measurement завершился
  source: SampleSource, // Network vs Cache (или Unknown)
}
```

`SampleSource`:
- `Network` — скачано из сети (учитываем в estimator).
- `Cache` — отдано из дискового/памятного кэша (НЕ учитываем).
- (опционально) `Unknown` — трактовать как `Network` только если гарантировано, что это сеть.

Контракт:
- `duration > 0`, иначе sample игнорируется.
- throughput в bps: `sample_bps = (bytes * 8) / duration_secs`.

### 1.3 Buffer model

ABR использует оценку “сколько у нас буфера”:
- `buffer_level_secs: f64` — сколько секунд медиа уже “внутри pipeline”.

Источник данных:
- может быть получен от драйвера HLS (prefetch depth × target duration),
- или от bridge/pipeline (если есть более точная метрика),
- или эвристикой.

Важно: **ABR не должен зависеть от точной модели буфера**, но должен уметь использовать её как gating.

---

## 2) Архитектура: Estimator + Controller + Driver apply

### 2.1 Разделение ответственности

**Estimator**:
- принимает throughput samples,
- хранит окно/состояние,
- выдаёт `estimate_bps: Option<u64>`.

**Controller**:
- принимает:
  - список variants + текущий variant,
  - `estimate_bps`,
  - `buffer_level_secs`,
  - состояние “когда последний раз свитчились”,
  - конфиг (safety factor, hysteresis, min interval),
- выдаёт `AbrDecision`.

**Driver/Worker**:
- применяет `AbrDecision` к фактической загрузке:
  - выбирает другой variant,
  - обеспечивает корректный переход (init segment / discontinuity),
  - только после применения эмитит event “applied”.

### 2.2 Decision vs Applied (обязательное правило)

ABR “решение” не равно фактическому применению.

- `decide()` может вернуть “переключиться на variant 2”.
- Но применение может быть отложено (например, пока не завершили текущий segment).
- События должны отражать обе стадии, если они существуют:
  - `VariantDecision` (telemetry, best-effort),
  - `VariantApplied` (строгая семантика: switch реально произошёл).

Если в проекте оставляют только одно событие — оно должно быть **applied**, иначе оно бесполезно для строгой логики.

---

## 3) Конфиг ABR (portable)

Рекомендуемый набор настроек (в духе legacy):

```text
AbrConfig {
  initial_variant_index: Option<usize>, // default Some(0)
  min_switch_interval: Duration,        // default 30s
  throughput_safety_factor: f64,        // default 1.5
  up_hysteresis_ratio: f64,             // default 1.3
  down_hysteresis_ratio: f64,           // default 0.8
  min_buffer_for_up_switch_secs: f64,   // default 10.0
  down_switch_buffer_secs: f64,         // default 5.0
  sample_window: Duration,              // default 30s
}
```

Смысл параметров:
- `throughput_safety_factor`: “насколько осторожны”. Чем больше, тем реже upswitch.
- `up_hysteresis_ratio`: дополнительный порог для upswitch.
- `down_hysteresis_ratio`: порог для downswitch.
- `min_switch_interval`: анти-осцилляция.
- `min_buffer_for_up_switch_secs`: не апсвитчиться при маленьком буфере.
- `down_switch_buffer_secs`: при буфере ниже — downswitch может быть агрессивным.
- `sample_window`: ограничение памяти estimator и “актуальности” измерений.

---

## 4) Estimator reference (EWMA + окно)

### 4.1 Требования

- Обновляется **только** по `SampleSource::Network`.
- Старые samples не должны бесконечно влиять (используй окно по времени + EWMA).
- Должен быть детерминируемым в тестах:
  - можно подставить “clock” (или передавать `at` явно),
  - окно должно отбрасывать samples по `at`.

### 4.2 Простейший EWMA

Пусть `x` — новый throughput sample (bps), `s` — текущее состояние.

```text
s_next = alpha * x + (1 - alpha) * s
```

Где `alpha` обычно 0.2..0.5.

Вариант “time-weighted” (опционально):
- `alpha` зависит от `duration`, чтобы короткие sample не доминировали.

Для portable реализации достаточно фиксированного `alpha`, если tests построены соответственно.

### 4.3 Окно samples

Даже если используешь EWMA, храни `last_update_at` и “сброс” при длинной паузе:
- если `now - last_update_at > sample_window` → можно уменьшить доверие/сбросить estimate.

---

## 5) Controller decision rules (portable, deterministic)

### 5.1 Основная формула эффективности

```text
effective_bps = estimate_bps / throughput_safety_factor
```

(эквивалентно `estimate_bps * (1.0 / safety_factor)`).

### 5.2 Candidate selection

Пусть:
- `variants_sorted_by_bandwidth` (возрастающе),
- `current = current_variant_index`,
- `current_bw = variants[current].bandwidth_bps`.

**Upswitch candidate**:
- самый высокий variant, удовлетворяющий:

```text
variant.bandwidth_bps * up_hysteresis_ratio <= effective_bps
```

и при этом:
- `buffer_level_secs >= min_buffer_for_up_switch_secs`,
- `min_switch_interval` соблюдён.

**Downswitch condition**:
- если:

```text
effective_bps < current_bw * down_hysteresis_ratio
```

или
- `buffer_level_secs <= down_switch_buffer_secs` (агрессивный downswitch).

Downswitch candidate:
- самый высокий variant ниже текущего, удовлетворяющий:

```text
variant.bandwidth_bps <= effective_bps
```

Если такого нет — выбираем самый низкий.

### 5.3 Guard rails

- Если `estimate_bps == None`:
  - либо `NoDecision` (остаться на текущем),
  - либо “fallback to initial/default” только при старте.
- Если `min_switch_interval` не прошёл:
  - `NoDecision(MinInterval)`.
- Если manual override активен:
  - `Decision(ManualOverride)` всегда побеждает (но всё равно может быть ограничение “не свитчить чаще”, по желанию; лучше фиксировать это тестами).

### 5.4 Output: AbrDecision

Минимально:

```text
AbrDecision {
  target_variant_index: usize,
  reason: AbrReason,
  changed: bool,
}
```

Рекомендуемые причины:

- `Initial`
- `ManualOverride`
- `UpSwitch`
- `DownSwitch`
- `MinInterval`
- `NoEstimate`
- `BufferTooLowForUpSwitch`
- `AlreadyOptimal`

Опционально:
- `require_init: bool` (если смена variant требует init segment/re-init decoder).

---

## 6) Интеграция с HLS worker (apply semantics)

Рекомендованный apply алгоритм в driver:

1) На границе сегмента (или “перед выбором следующего сегмента”) вызвать `controller.decide(...)`.
2) Если decision меняет variant:
   - сменить “selected variant” в worker state,
   - обеспечить корректный переход:
     - возможно, нужно вставить init segment,
     - продолжить с текущего segment index (без “restart” VOD, если это контракт).
3) Эмитить `VariantApplied { from, to, reason }` **после** фактического переключения.

Отдельно:
- Если событие “VariantDecision” существует — оно не должно использоваться для строгой логики.

---

## 7) Обязательные тесты (portable test plan)

Тесты должны быть:
- детерминированные,
- без внешней сети,
- не завязаны на “sleep на глаз”.

Рекомендация: тестируй ABR **в чистом виде** (estimator+controller) без полноценного HLS, плюс 1–2 интеграционных теста на worker apply.

### 7.1 Estimator tests

1) `cache_hit_does_not_affect_throughput`
- arrange: estimator пустой
- act: `push_sample(Cache, bytes=..., duration=...)`
- assert: `estimate == None` (или не изменился)

2) `network_sample_updates_estimate`
- arrange: estimator пустой
- act: `push_sample(Network, bytes=1000, duration=100ms)`
- assert: estimate примерно `80000 bps` (1000 bytes = 8000 bits / 0.1s = 80_000 bps)

3) `old_samples_are_dropped_by_window`
- arrange: два sample с `at` сильно в прошлом и “сейчас”
- act: push оба, затем request estimate with “now”
- assert: старый не влияет (в зависимости от реализации окна/сброса)

### 7.2 Controller tests (decision-only)

Фикстура variants:
- v0: 256_000 bps
- v1: 512_000 bps
- v2: 1_024_000 bps

1) `downswitch_on_low_throughput`
- current=v2, estimate=300_000 bps, safety=1.5 => effective=200_000
- assert: decision target=v0 (или v0 как минимальный), reason=DownSwitch

2) `upswitch_requires_buffer_and_hysteresis`
- current=v0, estimate=2_000_000 bps, effective~1_333_333
- при buffer=2s => NoDecision(BufferTooLowForUpSwitch)
- при buffer=20s => UpSwitch target=v2 (если проходит hysteresis)

3) `min_switch_interval_prevents_oscillation`
- current=v1, last_switch=now-1s, min_interval=30s
- estimate высокий/низкий, но assert: NoDecision(MinInterval)

4) `manual_override_wins`
- selector/manual sets target=v0 while estimate suggests upswitch
- assert: decision target=v0, reason=ManualOverride

### 7.3 Integration-ish tests (decision vs applied)

Минимум один тест на worker state machine:

`variant_applied_emitted_only_after_switch`
- arrange: worker запускается, ABR выдаёт решение на upswitch
- act: читаем события
- assert:
  - “applied” приходит только после того, как worker реально начал выдавать байты нового variant (проверяется по детерминированным префиксам сегментов/фикстуры).

---

## 8) Non-goals (v1)

Чтобы не разрасталось:
- mid-stream retry (это net concern и в v1 запрещено).
- сложные модели буфера на уровне PCM.
- ML/сложный scoring (resolution, codecs) — только если потребуется и будет тест.

---

## 9) Checklist для агента (выполнять по шагам)

1) Добавить/обновить `ThroughputSampleSource` и гарантировать “Network only updates”.
2) Вынести estimator/controller (если сейчас в одном файле).
3) Ввести `AbrDecision` + `AbrReason`.
4) Добавить unit tests из раздела 7 (минимум 3–4).
5) Привязать “decision → apply → VariantApplied event” в HLS worker (интеграционный тест).
6) `cargo fmt`, `cargo test -p kithara-hls`, `cargo clippy -p kithara-hls`.
