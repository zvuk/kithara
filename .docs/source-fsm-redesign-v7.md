# Source FSM Redesign v7 — Unified Core for File and HLS

> Supersedes `source-fsm-redesign-v6.md`.
>
> `v6` correctly moved the design away from a flat `wait_range` enum, but stayed too HLS-centric. `v7` defines one common source coordination model for all `StreamType`s, then maps `kithara-file` and `kithara-hls` onto that model.

## TL;DR

Правильный уровень абстракции здесь не `HlsSource`, а `Source` как таковой.

Нужен общий core FSM для любого source:

- lifecycle wait/read/seek coordination
- invalidation of stale demand work
- EOF / failure semantics
- interruption semantics during flush

А дальше каждый протокол добавляет свою policy:

- `File`: single-object layout, byte-range demand, no format boundary
- `Hls`: segmented layout, variant-aware demand, format boundary on variant change

Ключевой вывод:

- `HLS` не должен задавать правила для FSM
- `File` не должен жить по отдельной модели
- оба должны быть частными случаями одной source coordination model

При этом я бы аккуратно уточнил формулировку:

> `file` лучше считать не "надмножеством HLS", а вырожденным частным случаем общей модели линейного источника.

На уровне layout это "один сегмент". На уровне transfer behavior у `file` есть свои фазы (`Sequential -> GapFilling -> Complete`), которых у HLS нет в таком виде.

---

## 1. Что было не так в v6

`v6` уже ушел от плоского `SourcePhase`, но все еще строился вокруг проблем `kithara-hls`:

- `variant_fence`
- midstream switch
- committed variant
- segmented byte layout

Это правильно для stress-case анализа, но неправильно как архитектурная база.

Проблема:

- если core модель проектируется из HLS-специфики, то `kithara-file` остается "упрощенным исключением"
- если `file` остается исключением, то у нас снова две разные ментальные модели source
- это прямо противоречит цели сделать код компактнее и понятнее

---

## 2. Правильная постановка задачи

### Что должно быть общим

Для любого `Source` в Kithara должны действовать одинаковые правила:

1. `wait_range(range)` всегда классифицирует состояние как одно из:
   - ready
   - need data
   - interrupted
   - eof
   - failed
2. seek/flush всегда инвалидирует stale blocking work
3. pending on-demand work должно иметь явный invalidation token
4. EOF и failure должны быть отделены от "данных пока нет"
5. source не должен хранить per-call `Ready/Waiting` как долгоживущее поле

### Что может отличаться

Различаться могут только protocol policies:

- как устроен layout
- какой тип demand request нужен downloader'у
- есть ли decoder boundary / format fence
- какие downloader phases нужны для добычи данных

---

## 3. Общая модель: Core + Policy

Нужен один общий coordination core и тонкий policy-layer поверх него.

```text
                    Source Coordination
┌──────────────────────────────────────────────────────────┐
│                      Core FSM                            │
│  lifecycle + demand lifecycle + generation invalidation │
└──────────────────────────────────────────────────────────┘
                 ▲                              ▲
                 │                              │
        ┌────────┴────────┐            ┌────────┴────────┐
        │   File Policy   │            │    HLS Policy   │
        │ single object   │            │ segmented       │
        │ byte ranges     │            │ variants        │
        │ no fence        │            │ fence/switch    │
        └─────────────────┘            └─────────────────┘
```

То есть:

- один общий FSM
- не giant enum
- protocol-specific regions не определяют lifecycle, а лишь конкретизируют его

---

## 4. Core FSM, общий для File и HLS

### 4.1 CoreLifecycleState

Это состояние должно быть одинаковым для любого source.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreLifecycleState {
    Active,
    Flushing { seek_epoch: u64 },
    Eof { effective_total: u64 },
    Failed(FailureKind),
}
```

Смысл:

- `Active` — можно обычным образом ждать и читать
- `Flushing` — blocking wait должен быстро завершиться как interrupted
- `Eof` — EOF подтвержден на текущем effective layout
- `Failed` — дальше source continuation невозможен

Это одинаково верно и для file, и для hls.

### 4.2 CoreDemandState

Общий инвариант:

> любой pending demand должен быть привязан к generation token и быть явно инвалидируемым

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoreDemandState<K> {
    Idle,
    Resolving {
        generation: u64,
        miss_count: usize,
    },
    Pending {
        generation: u64,
        key: K,
        miss_count: usize,
    },
}
```

Где `K`:

- для `file` это `Range<u64>`
- для `hls` это `(variant, segment_index, seek_epoch, layout_generation)`

### 4.3 Core generation token

Нужно единое правило invalidation:

```rust
pub(crate) struct CoreSourceFsm<K> {
    lifecycle: CoreLifecycleState,
    demand: CoreDemandState<K>,
    generation: u64,
}
```

`generation` увеличивается на любом событии, которое делает pending demand потенциально stale.

Это может быть:

- новый seek
- layout reset
- protocol-specific switch
- fatal invalidation

Для `file` generation почти всегда меняется только на seek/restart.
Для `hls` generation меняется и на seek, и на midstream switch.

### 4.4 Derived wait classification

Как и в `v6`, `wait` outcome не хранится в FSM.

```rust
pub(crate) enum WaitStatus<R> {
    Ready,
    NeedData(R),
    Interrupted,
    Eof,
    Failed(FailureKind),
}
```

Важно:

- `Ready`/`Interrupted`/`Eof` — derived
- `CoreLifecycleState` — stored coordination state

---

## 5. Policy layer: что именно протокол задает сам

Поверх core FSM каждый source задает три policy-области:

1. `LayoutPolicy`
2. `DemandPolicy`
3. `BoundaryPolicy`

### 5.1 LayoutPolicy

Отвечает на вопрос:

> "Как source проецирует данные в линейное byte space?"

### 5.2 DemandPolicy

Отвечает на вопрос:

> "Если байтов нет, какой именно request нужно сформировать downloader'у?"

### 5.3 BoundaryPolicy

Отвечает на вопрос:

> "Есть ли format/decoder boundary, который запрещает читать дальше без recreate?"

Для `file` boundary policy почти пустая.
Для `hls` boundary policy принципиальна.

---

## 6. File как частный случай общей модели

`kithara-file` уже показывает, что lifecycle source/downloader там тоже stateful:

- `FileSource::wait_range()` делает demand before wait
- `FileDownloader` имеет explicit phases
- `Timeline` используется как общий source of truth для read/download positions

Это не "отдельный мир". Это тот же coordination problem в упрощенной layout model.

### 6.1 File layout policy

Для file layout предельно простой:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileLayoutState {
    Empty,
    Linear,
}
```

Смысл:

- `Empty` — байты еще не появились
- `Linear` — источник представляет один линейный объект

Никаких:

- variant switch
- fence
- reset of byte topology

То есть file действительно вырожденный случай общей linear-layout модели.

### 6.2 File demand policy

Для file demand unit — это byte range.

```rust
type FileDemandKey = Range<u64>;
```

`wait_range` на file:

1. проверяет `contains_range`
2. если диапазон не готов, формирует `NeedData(range.clone())`
3. queue request
4. блокируется на `StorageResource::wait_range`

То есть по общей модели:

- `WaitStatus::NeedData(range)` — общий случай
- специфика file только в том, что key = `Range<u64>`

### 6.3 File boundary policy

Для file boundary отсутствует:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileBoundaryState {
    None,
}
```

`ReadOutcome::VariantChange` никогда не возникает.
`clear_variant_fence()` no-op.

### 6.4 File downloader phases

У file есть свой protocol-specific transfer machine:

```rust
enum FileTransferState {
    Sequential,
    GapFilling,
    Complete,
}
```

Это не часть общего source core.

Это именно policy downloader-а:

- сначала stream from start
- потом range fill gaps
- потом complete

То есть file и hls обязаны жить по одинаковому core lifecycle, но не обязаны иметь одинаковые fetch phases.

---

## 7. HLS как другой частный случай общей модели

HLS использует тот же core FSM, но policy слой сложнее.

### 7.1 HLS layout policy

У HLS layout нелинеен по происхождению, но обязан выглядеть линейным для decoder.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HlsLayoutState {
    Empty,
    Stable { committed_variant: usize },
    ResetPending {
        target_variant: usize,
        seek_epoch: u64,
    },
    Switching {
        from_variant: usize,
        to_variant: usize,
        layout_generation: u64,
    },
}
```

Это extension к core, а не замена core.

### 7.2 HLS demand policy

У HLS demand unit — это не raw byte range, а segment target в конкретном layout context.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HlsDemandKey {
    pub generation: u64,
    pub seek_epoch: u64,
    pub variant: usize,
    pub segment_index: usize,
}
```

Именно здесь живут:

- committed lookup
- metadata lookup
- fallback segment estimate
- stale request invalidation on layout switch

### 7.3 HLS boundary policy

У HLS есть decoder boundary:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HlsBoundaryState {
    Open,
    Locked { decoder_variant: usize },
    PendingVariantChange {
        from_variant: usize,
        to_variant: usize,
    },
}
```

Это policy, а не core.

---

## 8. Что действительно должно стать общим

Если сделать общий redesign правильно, то общими должны быть не все enum values, а общие правила.

### Rule 1: wait is always classified the same way

Для любого source:

```text
Ready | NeedData(request) | Interrupted | Eof | Failed
```

Различается только тип `request`.

### Rule 2: seek invalidates stale work by generation

Для любого source:

- новый seek не должен оставлять старый pending demand валидным

Различие только в том, что:

- у `file` generation меняется редко
- у `hls` generation меняется еще и на layout switch

### Rule 3: flush interruption semantics are common

Для любого source:

- если `Timeline::is_flushing() == true`, `wait_range` не должен висеть вечно
- он должен вернуться как `Interrupted`

### Rule 4: EOF is not the same as "not ready yet"

Для любого source:

- отсутствие данных само по себе не EOF
- EOF требует явного подтверждения и effective total boundary

### Rule 5: failure is not the same as timeout

Для любого source:

- `timeout` — это caller-visible budget exhaustion
- `failed` — это machine state, в котором source больше не жизнеспособен

---

## 9. Где должен жить общий core

Если модель действительно общая, логично вынести core ближе к `kithara-stream`, а protocol-specific policy оставить в protocol crates.

### Предлагаемая декомпозиция

#### `kithara-stream`

Новый generic coordination module:

- `CoreSourceFsm<K>`
- `CoreLifecycleState`
- `CoreDemandState<K>`
- `WaitStatus<R>`
- `FailureKind`

Это не должно знать ничего про:

- variants
- segments
- playlists
- range requests implementation details

#### `kithara-file`

Добавляет:

- `FileLayoutState`
- `FileDemandKey = Range<u64>`
- `FileTransferState`

#### `kithara-hls`

Добавляет:

- `HlsLayoutState`
- `HlsDemandKey`
- `HlsBoundaryState`

### Почему это лучше

Так мы наконец получаем одну mental model:

- `Source` coordination одинаковая
- source-specific topology разная

---

## 10. Предлагаемая структура типов

Ниже не буквальный final API, а форма, в которую нужно целиться.

```rust
pub(crate) struct SourceCoord<P: SourcePolicy> {
    core: CoreSourceFsm<P::DemandKey>,
    layout: P::LayoutState,
    boundary: P::BoundaryState,
}

pub(crate) trait SourcePolicy {
    type DemandKey;
    type LayoutState;
    type BoundaryState;
}
```

### File mapping

```rust
struct FilePolicy;

impl SourcePolicy for FilePolicy {
    type DemandKey = Range<u64>;
    type LayoutState = FileLayoutState;
    type BoundaryState = FileBoundaryState;
}
```

### HLS mapping

```rust
struct HlsPolicy;

impl SourcePolicy for HlsPolicy {
    type DemandKey = HlsDemandKey;
    type LayoutState = HlsLayoutState;
    type BoundaryState = HlsBoundaryState;
}
```

---

## 11. How `wait_range` should look after v7

После `v7` любой source должен работать по одной и той же схеме:

1. build snapshot
2. ask core machine for `WaitStatus`
3. if `NeedData(request)`:
   - protocol policy materializes request
   - enqueue if not already valid
4. wait or return

### Generic shape

```rust
fn wait_range(&mut self, range: Range<u64>, timeout: Duration) -> StreamResult<WaitOutcome, E> {
    let started_at = Instant::now();

    loop {
        let snapshot = self.build_wait_snapshot(&range);
        let status = self.coord.classify_wait(snapshot);

        match status {
            WaitStatus::Ready => return Ok(WaitOutcome::Ready),
            WaitStatus::Interrupted => return Ok(WaitOutcome::Interrupted),
            WaitStatus::Eof => return Ok(WaitOutcome::Eof),
            WaitStatus::Failed(kind) => return Err(self.map_failure(kind)),
            WaitStatus::NeedData(request) => self.enqueue_if_needed(request)?,
        }

        if started_at.elapsed() > timeout {
            return Err(self.timeout_error(range.clone(), timeout));
        }

        self.block_until_progress_or_interrupt()?;
    }
}
```

### File specialization

- `build_wait_snapshot` checks `contains_range`
- `request = range.clone()`
- block on resource condvar

### HLS specialization

- `build_wait_snapshot` checks `DownloadState + Timeline`
- `request` resolved via committed/metadata/fallback segment logic
- block on `SharedSegments.condvar`

---

## 12. What changes in HLS after v7

HLS still needs the richest policy, but теперь он не "главный source", а просто самый сложный instantiation.

### HLS-specific pieces that remain

- segmented layout
- variant switch
- decoder boundary
- seek preserve/reset logic
- metadata-vs-committed fallback

### HLS-specific pieces that should stop leaking into core reasoning

- `had_midstream_switch`
- `variant_fence`
- local `WaitRangeState`

Они должны стать policy state, встроенным в unified coordination model.

---

## 13. What changes in File after v7

File не должен оставаться ad-hoc "простым исключением".

### File should explicitly participate in the same model

Нужно, чтобы у file был тот же conceptual path:

- common lifecycle state
- common demand invalidation semantics
- common interruption semantics

Даже если реализации проще:

- no boundary state
- no segmented layout
- demand key is just a byte range

### Consequence

Если потом появится третий source type, он уже не будет выбирать между:

- "как file"
- "как hls"

Он будет реализовывать:

- общий source core
- свою policy

---

## 14. Important nuance: file is not literally a superset of HLS

Ваше направление правильное, но я бы зафиксировал термин точнее.

### На уровне layout

Да:

- `file` можно мыслить как один линейный объект
- `hls` как несколько объектов, stitched into one linear object

В этом смысле `file` — вырожденный случай общей модели layout.

### На уровне fetch/transfer phases

Нет:

- у `file` есть специализированная стратегия `Sequential -> GapFilling -> Complete`
- у `hls` есть variant planning, switch handling, tail backfill, segment commits

То есть:

- единая source coordination model — да
- единая downloader phase machine — не обязательно

Это важная граница, чтобы не переобобщить дизайн.

---

## 15. Migration plan

### Phase 1: Formalize the common core

Добавить общий design module в `kithara-stream`:

- `CoreLifecycleState`
- `CoreDemandState`
- `FailureKind`
- `WaitStatus`

На этом шаге поведение не меняется.

### Phase 2: Map file to the core

Сначала адаптировать `kithara-file`, потому что это simpler protocol.

Почему именно file first:

- там проще проверить, что core действительно общий
- если core слишком HLS-specific, на file это сразу станет видно

Нужно:

- добавить file policy mapping
- выразить `wait_range` через `WaitStatus::NeedData(range)`
- описать interruption/EOF/failure через core rules

### Phase 3: Map HLS to the same core

Только после этого переводить HLS.

Нужно:

- заменить `WaitRangeState`
- заменить `had_midstream_switch`
- заменить `variant_fence`
- встроить HLS policy states в общий coordination shape

### Phase 4: Align docs and tests

Обновить:

- `kithara-stream/README.md`
- `kithara-file/README.md`
- `kithara-hls/README.md`

И добавить cross-protocol tests на одинаковые core invariants.

---

## 16. Test matrix for v7

Нужны не только protocol-specific tests, но и общие invariants.

### Shared invariant tests

- `wait_range_returns_interrupted_while_flushing`
- `pending_demand_is_invalidated_by_generation_change`
- `eof_is_distinct_from_not_ready`
- `failed_state_stops_wait_loop`

### File-specific tests

- missing range becomes `NeedData(range)`
- gap-filling does not violate core lifecycle
- complete file returns EOF only past total

### HLS-specific tests

- same-variant seek preserves layout under the same core lifecycle
- midstream switch bumps generation and invalidates stale demand
- incompatible variant read produces boundary transition

---

## 17. Final design decision

`v7` fixes the main architectural mistake of `v6`:

- core FSM is no longer HLS-shaped
- `file` is no longer an afterthought
- protocol crates define policy, not lifecycle

The target architecture is:

```text
kithara-stream
  └── generic source coordination core

kithara-file
  └── single-object policy over the core

kithara-hls
  └── segmented/variant-aware policy over the same core
```

Именно это делает систему реально компактнее:

- одна общая mental model
- один набор invariants
- разные protocol policies

а не две независимые логики, одна "простая", другая "сложная".
