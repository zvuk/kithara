# Source FSM Redesign v6 — Composite FSM with Nested Regions

> Supersedes `source-fsm-redesign-v5.md`.
>
> `v5` was a valid local cleanup of `wait_range`, but not a full redesign of source coordination. `v6` moves the design to the right abstraction level: one owning state machine with nested regions, explicit events, and derived wait classification.

## TL;DR

Нужен не один плоский `SourcePhase`, а один `SourceFsm`, внутри которого живут несколько маленьких подмашин:

- `LifecycleState` — source сейчас активен, flushing, EOF или failed
- `LayoutState` — какой committed byte layout сейчас считается истинным
- `DemandState` — есть ли валидный on-demand запрос для текущего seek/switch контекста
- `FenceState` — может ли decoder читать текущий variant без recreate

Ключевая идея:

- храним только долгоживущую coordination state
- не храним `Ready/NeedData/Eof` как field
- `WaitStatus` вычисляется на каждом `wait_range(range)` из:
  - snapshot текущих байтов/offsets
  - `Timeline`
  - `SourceFsm`

Это позволяет убрать специальные флаги и implicit transitions:

- `WaitRangeState`
- `had_midstream_switch: AtomicBool`
- `variant_fence: Option<usize>`

и заменить их одной явной моделью переходов.

---

## 1. Проблема, которую должен решать redesign

Сейчас сложность размазана между тремя уровнями:

1. `Timeline` в `kithara-stream`
2. `HlsSource` и `wait_range`
3. `HlsDownloader`

Из-за этого реальные переходы происходят через набор независимых флагов и косвенных эффектов:

- `timeline.is_flushing()`
- `timeline.eof()`
- `timeline.seek_epoch()`
- `had_midstream_switch`
- `variant_fence`
- `on_demand_request_epoch`
- `last_committed_variant`
- `download_position`

Это делает код трудным для чтения по двум причинам:

1. Важные инварианты нигде не названы явно.
2. Смена состояния происходит не в одном месте, а как цепочка побочных эффектов на разных слоях.

### Симптомы в текущем коде

- `wait_range()` принимает решение не только по "есть байты / нет байтов", но и по seek epoch, EOF, flush state, stopped, eviction, midstream switch.
- `had_midstream_switch` существует только потому, что pending on-demand request может устареть внутри того же `seek_epoch`.
- `variant_fence` защищает decoder contract, но живет отдельно от source coordination.
- `set_seek_epoch()` и `seek_time_anchor()` вместе образуют один transition, но реализованы как две разные операции в разных точках lifecycle.

---

## 2. Почему v5 недостаточен

`v5` предлагал заменить:

- `WaitRangeState`
- `WaitRangeContext`
- `WaitRangeDecision`

на один плоский enum:

```rust
enum SourcePhase {
    Waiting,
    Ready,
    Seeking,
    Eof,
    Failed,
}
```

Это улучшает локальный код `wait_range`, но не решает системную проблему.

### Почему

`Waiting/Ready/Seeking/Eof/Failed` — это не полное состояние source, а лишь ответ на вопрос:

> "Что делать с этим конкретным `range` прямо сейчас?"

То есть это не lifecycle state, а derived classification.

Если сохранить такой `phase` в `HlsSource`, мы материализуем вычислимое состояние и получим новую категорию багов:

- stale `phase`
- неочевидный owner переходов
- расхождение между `phase` и реальным `Timeline/DownloadState`

### Главный вывод

`Ready`, `Interrupted`, `Eof` должны остаться вычисляемым результатом `wait_range`, а не стать полем source.

---

## 3. Цели v6

### Основные цели

- Сделать все долгоживущие coordination states явными.
- Дать одному объекту ownership над transition logic.
- Убрать implicit coupling между source и downloader через ad-hoc flags.
- Свести `wait_range()` к схеме:
  - build snapshot
  - classify
  - maybe queue demand
  - wait
- Сохранить существующий `Source` trait API.

### Не-цели

- Не переписывать `Timeline`.
- Не менять `Stream<T>` API.
- Не менять `Audio` seek protocol.
- Не делать giant enum со всеми комбинациями под-состояний.
- Не тащить в FSM `DownloadState` или playlist metadata как "state"; это данные, а не coordination phase.

---

## 4. Архитектурный принцип

Нужен один composite FSM, а не один flat enum.

### Неправильно

```rust
enum SourceState {
    WaitingWithPendingDemandAfterSwitch,
    WaitingMetadataFallbackAfterResetSeek,
    ReadyButFenceBlocked,
    ...
}
```

Это мгновенно приведет к state explosion.

### Правильно

```rust
pub(crate) struct SourceFsm {
    lifecycle: LifecycleState,
    layout: LayoutState,
    demand: DemandState,
    fence: FenceState,
    switch_generation: u64,
}
```

ASCII-схема:

```text
SourceFsm
├── lifecycle  : общий lifecycle source/downloader coordination
├── layout     : committed byte layout
├── demand     : валидность on-demand request
├── fence      : decoder/read boundary
└── switch_gen : monotonic invalidation token for midstream switch
```

Это один FSM на уровне ownership и transition API, но внутри у него несколько orthogonal regions.

---

## 5. Что хранить, а что вычислять

### Хранить в FSM

Только coordination state, которое:

- живет дольше одного вызова `wait_range`
- должно переживать переход между source/downloader/decoder
- нельзя безопасно "догадать" из одного поля

### Не хранить в FSM

Не нужно хранить:

- `range_ready`
- `effective_total`
- `should_return_eof`
- `should_interrupt`
- `WaitOutcome`

Все это должно быть derived per-call classification.

### Derived result

`wait_range()` должен работать через отдельный derived enum:

```rust
pub(crate) enum WaitStatus {
    Ready,
    NeedData(DemandTarget),
    Interrupted,
    Eof,
    Failed(FailureKind),
}
```

`WaitStatus` не хранится в `SourceFsm`.

---

## 6. Предлагаемая модель состояний

### 6.1 LifecycleState

Отвечает на вопрос:

> "Можно ли source продолжать обычный read/wait lifecycle?"

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleState {
    Active,
    Flushing { seek_epoch: u64 },
    Eof { effective_total: u64 },
    Failed(FailureKind),
}
```

Смысл:

- `Active` — обычная работа
- `Flushing` — seek начат, blocking waits должны вернуться как interrupted
- `Eof` — подтвержден EOF для текущего effective layout
- `Failed` — source больше не должен продолжать wait/read loop

### 6.2 LayoutState

Отвечает на вопрос:

> "Какая byte layout сейчас считается committed truth?"

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayoutState {
    Empty,
    Stable { committed_variant: usize },
    ResetPending {
        target_variant: usize,
        seek_epoch: u64,
    },
    Switching {
        from_variant: usize,
        to_variant: usize,
        switch_generation: u64,
    },
}
```

Смысл:

- `Empty` — еще нет committed layout
- `Stable` — layout согласован и читаем как единый truth
- `ResetPending` — seek уже решил, что layout нужно пересобрать, но новый steady state еще не подтвержден commit'ом
- `Switching` — произошел midstream switch, старый demand context надо инвалидировать

### 6.3 DemandState

Отвечает на вопрос:

> "Есть ли уже валидный on-demand request для текущего seek/switch контекста?"

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DemandState {
    Idle,
    Resolving {
        seek_epoch: u64,
        switch_generation: u64,
        miss_count: usize,
    },
    Pending {
        seek_epoch: u64,
        switch_generation: u64,
        variant: usize,
        segment_index: usize,
        miss_count: usize,
    },
}
```

Смысл:

- `Idle` — demand не активен
- `Resolving` — source пытается вывести segment target из committed data / metadata / fallback
- `Pending` — request уже запушен и считается валидным только если совпадают:
  - `seek_epoch`
  - `switch_generation`

Это заменяет:

- `WaitRangeState::on_demand_request_epoch`
- `metadata_miss_count`
- `had_midstream_switch` как неявную invalidation side-channel

### 6.4 FenceState

Отвечает на вопрос:

> "Может ли текущий decoder безопасно читать variant, который source видит в committed segments?"

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FenceState {
    Open,
    Locked { decoder_variant: usize },
    PendingVariantChange {
        from_variant: usize,
        to_variant: usize,
    },
}
```

Смысл:

- `Open` — decoder еще не зафиксирован на variant
- `Locked` — decoder сейчас валиден только для конкретного variant
- `PendingVariantChange` — source наткнулся на incompatible variant; следующий `read_at` должен вернуть `VariantChange`, пока audio pipeline не вызовет `clear_variant_fence()`

Это замена для `variant_fence: Option<usize>`.

### 6.5 FailureKind

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    Cancelled,
    DownloaderStopped,
    MetadataExhausted,
}
```

Не нужно хранить весь `HlsError` в FSM. Достаточно короткой classification, а conversion в `HlsError` оставить на краю API.

---

## 7. Где должен жить SourceFsm

### Предложение

Хранить `SourceFsm` в shared coordination state, доступном и source, и downloader:

```rust
pub struct SharedSegments {
    pub segments: Mutex<DownloadState>,
    pub condvar: Condvar,
    pub timeline: Timeline,
    pub reader_advanced: Notify,
    pub segment_requests: SegQueue<SegmentRequest>,
    pub playlist_state: Arc<PlaylistState>,
    pub cancel: CancellationToken,
    pub stopped: AtomicBool,
    pub current_segment_index: Arc<AtomicU32>,
    pub abr_variant_index: Arc<AtomicUsize>,
    pub coord: Mutex<SourceFsm>,
}
```

### Почему не поле `HlsSource`

Если `SourceFsm` лежит только в `HlsSource`, то downloader все равно будет менять lifecycle/layout косвенно через side flags.

Тогда:

- owner переходов снова будет размазан
- `had_midstream_switch` никуда не денется
- `wait_range` станет красивее, но не проще как система

### Locking rule

Важно не допустить нового lock cycle.

Правило:

- `DownloadState` и `SourceFsm` никогда не держатся одновременно
- source/downloader сначала строят snapshot
- потом берут `coord` lock
- потом отпускают lock
- только затем делают side effect:
  - push queue
  - condvar notify
  - network plan

То есть FSM принимает решение, но не держит lock во время I/O и не живет внутри `DownloadState`.

---

## 8. События FSM

Наружу FSM должен иметь один небольшой transition API. Не обязательно один `enum SourceEvent`, но логически события должны быть именно такими.

### События source-side

```rust
begin_seek(seek_epoch, target_variant, segment_index, reset_layout)
finish_seek(seek_epoch)
classify_wait(snapshot) -> WaitStatus
on_demand_queued(seek_epoch, switch_generation, variant, segment_index)
on_demand_invalidated(reason)
on_read_variant(variant) -> ReadDecision
clear_fence()
on_cancel()
on_downloader_stopped()
```

### События downloader-side

```rust
on_seek_reset(seek_epoch, variant, segment_index, preserved_layout)
on_midstream_switch(from_variant, to_variant)
on_segment_committed(variant, segment_index, byte_offset, end_offset)
on_eof(effective_total)
on_failure(FailureKind)
```

### Snapshot input

`classify_wait` не должен читать mutex/atomics сам. Он должен получать snapshot:

```rust
pub(crate) struct WaitSnapshot {
    pub flushing: bool,
    pub stopped: bool,
    pub cancelled: bool,
    pub eof: bool,
    pub seek_epoch: u64,
    pub effective_total: u64,
    pub range_start: u64,
    pub range_ready: bool,
    pub current_variant: usize,
}
```

Это делает machine pure и тестируемой.

---

## 9. Derived wait classification

`wait_range()` должен превратиться в thin loop вокруг `classify_wait(snapshot)`.

### Принцип

FSM не отвечает на вопрос "есть ли байты в `DownloadState`".

FSM отвечает на вопрос:

> "Если байтов нет, является ли это normal wait, seek interruption, EOF, failure или валидный demand request?"

### Псевдокод

```rust
fn wait_range(&mut self, range: Range<u64>, timeout: Duration) -> StreamResult<WaitOutcome, HlsError> {
    let started_at = Instant::now();

    loop {
        let snapshot = self.build_wait_snapshot(&range);

        let status = {
            let mut fsm = self.shared.coord.lock_sync();
            fsm.classify_wait(snapshot)
        };

        match status {
            WaitStatus::Ready => return Ok(WaitOutcome::Ready),
            WaitStatus::Interrupted => return Ok(WaitOutcome::Interrupted),
            WaitStatus::Eof => return Ok(WaitOutcome::Eof),
            WaitStatus::Failed(kind) => return Err(self.map_failure(kind)),
            WaitStatus::NeedData(target) => {
                self.queue_demand_if_needed(target)?;
            }
        }

        if started_at.elapsed() > timeout {
            return Err(StreamError::Source(HlsError::Timeout(...)));
        }

        self.wait_on_condvar_or_interrupt()?;
    }
}
```

### Важное отличие от v5

`NeedData` не равно `Waiting`.

Это отдельная coordination outcome:

- ждать пассивно
- или активно push'нуть demand
- или инвалидировать старый demand

Именно это сейчас размазано между `WaitRangeState`, `had_midstream_switch` и helper-функциями.

---

## 10. Demand invalidation rules

Это одна из ключевых частей `v6`.

Pending request считается валидным только если одновременно совпадают:

- `seek_epoch`
- `switch_generation`

### Значит demand надо инвалидировать при

1. новом seek
2. midstream switch
3. подтвержденном EOF
4. явном reset layout
5. fatal failure

### Это заменяет текущий fragile path

Сейчас логика такая:

- downloader drain'ит `segment_requests`
- ставит `had_midstream_switch = true`
- source в `wait_range` делает `swap(false)`
- после этого локально чистит `on_demand_pending`

В `v6` вместо этого:

- downloader делает `switch_generation += 1`
- FSM переводит `layout -> Switching`
- любой `DemandState::Pending` со старым generation автоматически stale

То есть invalidation становится декларативной, а не процедурной.

---

## 11. Fence / decoder boundary redesign

`variant_fence` нельзя оставлять вне FSM, если документ претендует на полноценный redesign.

### Текущее поведение

- первый `read_at` lock'ит fence на текущем variant
- crossing на совместимый codec может silently move fence
- crossing на incompatible variant возвращает `VariantChange`
- audio pipeline потом вручную вызывает `clear_variant_fence()`

### В v6

Это становится явной machine region:

```text
Open
  └─ first read on variant V ─▶ Locked(V)

Locked(V)
  ├─ read variant V ─▶ Locked(V)
  ├─ compatible switch V -> W ─▶ Locked(W)
  └─ incompatible switch V -> W ─▶ PendingVariantChange(V, W)

PendingVariantChange(V, W)
  ├─ read_at ─▶ return VariantChange
  └─ clear_fence() from audio pipeline ─▶ Open
```

Это делает decoder contract видимым в state model, а не только в `read_at` if-chain.

---

## 12. Seek flow in v6

Seek остается orchestrated через `Timeline`.

`Timeline` по-прежнему источник истины для:

- `seek_epoch`
- `flushing`
- `seek_target`
- `seek_pending`

`SourceFsm` не должен дублировать этот ownership.

### Полный flow

```text
Audio::seek()
  ├─ timeline.initiate_seek(epoch)
  ├─ source.notify_waiting()
  └─ worker wakes

StreamAudioSource::apply_pending_seek()
  ├─ source.set_seek_epoch(epoch)
  ├─ source.clear_variant_fence()
  ├─ source.seek_time_anchor(position)
  │    ├─ resolve anchor
  │    ├─ classify seek as Preserve or Reset
  │    ├─ fsm.begin_seek(...)
  │    ├─ update layout state
  │    └─ queue target demand
  ├─ timeline.complete_seek(epoch)
  └─ source.notify_waiting()
```

### Mapping to FSM

- `timeline.initiate_seek()` не меняет FSM напрямую
- `set_seek_epoch()` и `seek_time_anchor()` переводят FSM в:
  - `lifecycle = Flushing { epoch }`
  - `layout = ResetPending` или `Stable`
  - `demand = Idle` перед новой классификацией
  - `fence = Open`

После `timeline.complete_seek(epoch)`:

- `fsm.finish_seek(epoch)` переводит `lifecycle` обратно в `Active`, если нет EOF/failure

---

## 13. Midstream switch flow in v6

Это второй критический сценарий после seek.

### Текущее состояние

Midstream switch сейчас влияет сразу на:

- `segment_requests`
- `had_midstream_switch`
- `resolve_byte_offset()`
- `fence_at()`
- `wait_range` re-push logic

### В v6

Нужен один явный transition:

```rust
fsm.on_midstream_switch(from_variant, to_variant)
```

Эффекты:

1. `switch_generation += 1`
2. `layout = Switching { from, to, switch_generation }`
3. `demand = Idle`
4. если `fence = Locked { decoder_variant: from }`, то fence сам по себе не открывается
5. downloader по-прежнему может drain'ить queue, но это теперь не semantic signal, а техническая деталь

### После первого commit нового variant

```rust
fsm.on_segment_committed(to_variant, segment_index, ...)
```

Переводит:

- `layout -> Stable { committed_variant: to_variant }`

Если switch был incompatible по codec, следующий `read_at` переведет fence в `PendingVariantChange`.

---

## 14. EOF semantics in v6

`Eof` нельзя считать просто по `timeline.eof()`.

Нужно сохранить текущий строгий инвариант:

> `WaitOutcome::Eof` возвращается только если:
>
> - timeline действительно в EOF
> - effective total известен и больше нуля
> - `range.start >= effective_total`

### В FSM

`LifecycleState::Eof { effective_total }` разрешается только после явного `on_eof(effective_total)`.

Но `classify_wait(snapshot)` все равно перепроверяет snapshot, а не слепо верит stored state.

Почему:

- `effective_total` зависит от runtime snapshot
- `total_bytes` может reconcile'иться после DRM size correction
- stale EOF state хуже, чем derived EOF classification

Поэтому `LifecycleState::Eof` полезен как coordination marker, но не заменяет snapshot-проверку.

---

## 15. Что уходит из кода

### Удаляется полностью

- `WaitRangeState`
- `had_midstream_switch: AtomicBool`
- `variant_fence: Option<usize>`

### Перестает быть semantic state

- `segment_requests` остается queue transport, но не является source of truth о pending demand

### Остается

- `Timeline`
- `DownloadState`
- `SeekLayout` concept, но он теперь должен входить в FSM transition API

---

## 16. Предлагаемые изменения по файлам

### Новый файл

- `crates/kithara-hls/src/source_fsm.rs`

Содержит:

- `SourceFsm`
- `LifecycleState`
- `LayoutState`
- `DemandState`
- `FenceState`
- `FailureKind`
- `WaitStatus`
- `WaitSnapshot`
- чистые transition helpers

### Изменяемые файлы

- `crates/kithara-hls/src/source.rs`
  - убрать `variant_fence`
  - использовать `coord: Mutex<SourceFsm>`
  - `read_at()` через `FenceState`
  - `seek_time_anchor()` через `fsm.begin_seek(...)`
- `crates/kithara-hls/src/source_wait_range.rs`
  - оставить snapshot builder
  - убрать `WaitRangeState/Context/Decision`
  - свести к `classify_wait + maybe_queue_demand`
- `crates/kithara-hls/src/downloader.rs`
  - убрать `had_midstream_switch`
  - все invalidation path вести через `fsm.on_midstream_switch(...)`
  - commit path уведомляет FSM
- `crates/kithara-hls/src/internal.rs`
  - инициализация `SourceFsm`
- `crates/kithara-hls/README.md`
  - обновить раздел seek/wait_range contract

### Без изменений по API

- `Source` trait
- `Stream<T>`
- `Audio<Stream<T>>`

---

## 17. Пошаговый план миграции

### Phase 1: Introduce machine without behavior changes

1. Добавить `source_fsm.rs`
2. Добавить `coord: Mutex<SourceFsm>` в `SharedSegments`
3. Инициализировать machine в базовом состоянии:
   - `lifecycle = Active`
   - `layout = Empty`
   - `demand = Idle`
   - `fence = Open`
   - `switch_generation = 0`
4. Написать unit tests только на чистые transitions

### Phase 2: Move demand invalidation into FSM

1. Перенести `WaitRangeState` semantics в `DemandState`
2. Добавить `WaitSnapshot`
3. Переписать `wait_range()` на `classify_wait(snapshot)`
4. Пока временно сохранить старый `had_midstream_switch`, но зеркалить его в `switch_generation`

### Phase 3: Replace switch flag with generation token

1. Убрать `had_midstream_switch`
2. Переписать downloader switch path на `fsm.on_midstream_switch(...)`
3. Переписать `wait_range` re-push logic на проверку generation
4. Обновить regression tests про midstream switch

### Phase 4: Replace variant_fence with FenceState

1. Убрать `variant_fence: Option<usize>`
2. Встроить fence transitions в `read_at`
3. `clear_variant_fence()` перевести на `fsm.clear_fence()`
4. Обновить tests вокруг `VariantChange`

### Phase 5: Collapse old helpers

Удалить:

- `WaitRangeState`
- `WaitRangeContext`
- `WaitRangeDecision`
- локальные if-цепочки, которые теперь становятся method calls на FSM

### Phase 6: Documentation and cleanup

1. Обновить README
2. Добавить ASCII diagram в crate docs
3. Убедиться, что все invariants покрыты тестами

---

## 18. Тест-план

### Unit tests для `source_fsm.rs`

- `classify_wait_returns_ready_when_snapshot_is_ready`
- `classify_wait_returns_interrupted_when_flushing`
- `pending_demand_is_invalidated_by_new_seek_epoch`
- `pending_demand_is_invalidated_by_switch_generation_bump`
- `midstream_switch_moves_layout_to_switching`
- `first_commit_after_switch_restores_stable_layout`
- `locked_fence_moves_to_pending_variant_change_on_incompatible_read`
- `clear_fence_returns_fence_to_open`

### Integration tests в `kithara-hls`

- same-variant seek preserves layout and does not recreate stale demand
- cross-variant seek enters reset path and queues target segment
- midstream switch drains effective demand without deadlock
- EOF is returned only past effective total
- ephemeral eviction still triggers re-fetch and does not leave FSM in stale pending state
- decoder fence returns `VariantChange` exactly once per incompatible boundary until cleared

### Existing regressions that must stay green

- tests around `reset_for_seek_epoch_preserves_total_bytes`
- tests around `handle_midstream_switch_notifies_condvar_for_repush`
- tests around stale fetch epoch dropping
- tests around same-variant seek watermark preservation

---

## 19. Почему это лучше v5

### Что реально улучшается

- исчезает special-case flag `had_midstream_switch`
- исчезает локальная state bag `WaitRangeState`
- исчезает неявный decoder barrier через `Option<usize>`
- invalidate/re-push semantics становятся декларативными
- transitions source/downloader можно тестировать как чистую state machine

### Что не делается "магией"

`v6` не обещает, что code станет короче любой ценой.

Он обещает более важное:

- меньше скрытых зависимостей
- меньше cross-layer угадывания
- более читаемый ownership переходов

Если итоговый diff окажется длиннее `v5`, но уберет implicit invariants, это хороший компромисс.

---

## 20. Риски и ограничения

### Риск 1: over-modeling

Если начать тащить в FSM:

- `DownloadState`
- playlist offsets
- throughput / ABR policy

то redesign станет хуже текущего кода.

Решение:

- FSM хранит только coordination state
- data остается в существующих структурах

### Риск 2: lock complexity

Новый `coord` mutex может создать deadlock при смешении с `segments` mutex.

Решение:

- формальный lock order rule: `segments` и `coord` не держим одновременно
- machine работает по snapshot input

### Риск 3: дублирование `Timeline`

Если `LifecycleState::Flushing` начнет конкурировать с `timeline.is_flushing()`, появятся два источника истины.

Решение:

- `Timeline` остается source of truth для seek/flush epoch
- FSM only mirrors coordination intent and derived consequences

---

## 21. Критерии готовности v6

`v6` можно считать реализованным только если выполнены все пункты:

1. В коде больше нет `had_midstream_switch: AtomicBool`.
2. В коде больше нет `variant_fence: Option<usize>`.
3. `wait_range()` больше не использует ad-hoc local state struct с семантикой coordination.
4. Pending demand валиден только по `(seek_epoch, switch_generation)`.
5. Midstream switch invalidates pending demand без `swap(false)` handshake.
6. Decoder boundary выражен как explicit state, а не как implicit optional field.
7. `WaitOutcome::{Ready, Interrupted, Eof}` вычисляются из snapshot + machine, а не хранятся как source field.
8. Все существующие seek/switch/EOF regressions проходят.

---

## 22. Итоговое решение

Да, несколько маленьких FSM можно и нужно "запаковать в один".

Но этот "один" должен быть:

- не giant enum
- а composite FSM с nested regions
- с одним owner transition logic
- с derived wait classification

Именно это дает шанс реально упростить `kithara-hls`, а не просто переименовать текущие if-цепочки.
